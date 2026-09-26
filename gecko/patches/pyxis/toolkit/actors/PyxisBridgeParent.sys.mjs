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

// In-memory subscription table: browsingContextId -> Set("service:event").
// Used so Echo signal fan-out (once wired) only pushes to actors that
// actually asked for a given service:event pair (PyxisBridgeChild's
// _addEventListener sends "PyxisBridge:Subscribe" on first listener add).
const gSubscriptions = new Map();

// Process-wide table of live PyxisBridgeParent instances, keyed by
// browsingContext id, so a Signal message arriving on ANY actor's Echo
// connection can be fanned out to every subscribed actor rather than only
// the one that happens to own that particular socket. Populated in
// actorCreated()/didDestroy() (see below). This is what
// forwardEventToContent's TODO asked for: a real registry to dispatch
// through once the transport exists.
const gLiveActors = new Map();

// Process-wide cache of app-id -> declared permissions[], populated by
// _getDeclaredPermissions() below. Deliberately process-lifetime (not
// per-actor): every pyxis:// document for the same app-id shares the same
// declared permission set, so there is nothing actor-specific to key on.
const gPermissionsCache = new Map();

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
    this._connectPromise = this._connect();
    return this._connectPromise;
  }

  async _connect() {
    const path = echoSocketPath();
    tracingLog("Connecting to Echo daemon at", path.path);

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

    this._waitForOutput();
    this._waitForInput();

    // Fire-and-forget handshake, matching every existing Rust Echo client's
    // behavior (see file-level notice: Echo's daemon does not gate
    // MethodCall on it, but sending one keeps this client's connection
    // behavior consistent with and visible the same way as the others in
    // the daemon's logs).
    try {
      await this._sendHandshake();
    } catch (e) {
      console.error("[PyxisBridge] Echo handshake failed:", e);
      // Non-fatal: proceed anyway, since MethodCall does not require it.
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
    const response = await this._sendAndAwait(message, /* timeoutMs */ 25000);
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
    const json = JSON.stringify(message);
    const bodyBytes = stringToUTF8Bytes(json);
    const framed = new Uint8Array(4 + bodyBytes.length);
    writeU32LE(framed, 0, bodyBytes.length);
    framed.set(bodyBytes, 4);
    this._writeQueue.push(framed);
    this._flushOutgoing();
  }

  _waitForOutput() {
    if (this._closed || !this._output) {
      return;
    }
    this._output.asyncWait(
      { onOutputStreamReady: () => this._flushOutgoing() },
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
          this._waitForOutput();
          return;
        }
        this._writeQueue.shift();
      } catch (e) {
        if (e.result === Cr.NS_BASE_STREAM_WOULD_BLOCK) {
          this._waitForOutput();
          return;
        }
        this._failAllPending(e);
        return;
      }
    }
  }

  // --- Inbound framing/read pump ---

  _waitForInput() {
    if (this._closed || !this._rawInput) {
      return;
    }
    this._rawInput.asyncWait(
      { onInputStreamReady: () => this._onInputReady() },
      0,
      0,
      Services.tm.currentThread
    );
  }

  _onInputReady() {
    if (this._closed) {
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
      this._waitForInput();
    } catch (e) {
      if (e.result === Cr.NS_BASE_STREAM_WOULD_BLOCK) {
        this._waitForInput();
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
    this._closed = true;
    try {
      this._rawInput?.close();
    } catch (e) {
      // ignore
    }
    try {
      this._output?.close();
    } catch (e) {
      // ignore
    }
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
// Echo signals are broadcast, not per-connection-filtered (see
// echo/src/signals.rs's SignalBus::broadcast and daemon.rs's handling of
// MessageType::Signal, which just calls signal_bus().broadcast() with no
// subscriber-list concept on the wire) — so EVERY EchoConnection that has
// ever sent a Signal message would in principle see it echoed back only if
// it re-subscribes; determining whether unsolicited SIGNAL messages arrive
// on a plain method-call connection without an explicit subscribe step is
// left for the follow-up wiring this stretch-goal section describes below.
// For now, any SIGNAL message that does arrive on a connection is fanned
// out to every live actor whose subscription table matches
// interface:method, mirroring gSubscriptions' existing per-actor filtering.
function dispatchSignalToSubscribers(message) {
  const service = message.header.interface?.replace(/^org\.geckoOS\./, "");
  const eventName = message.header.method;
  if (!service || !eventName) {
    return;
  }
  const key = `${service}:${eventName}`;
  for (const actor of gLiveActors.values()) {
    const bcId = actor.browsingContext?.id;
    if (bcId != null && gSubscriptions.get(bcId)?.has(key)) {
      actor.forwardEventToContent(service, eventName, message.body);
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

export class PyxisBridgeParent extends JSWindowActorParent {
  actorCreated() {
    const bcId = this.browsingContext?.id;
    if (bcId != null) {
      gLiveActors.set(bcId, this);
    }
  }

  didDestroy() {
    const bcId = this.browsingContext?.id;
    if (bcId != null) {
      gLiveActors.delete(bcId);
      gSubscriptions.delete(bcId);
    }
  }

  async receiveMessage(msg) {
    switch (msg.name) {
      case "PyxisBridge:CallService":
        return this._handleCallService(msg.data);
      case "PyxisBridge:Subscribe":
        this._handleSubscribe(msg.data);
        return undefined;
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
    const declared = await this._getDeclaredPermissions(appId);
    if (capability && !declared.includes(capability)) {
      throw new Error(
        `Pyxis capability denied: app '${appId}' did not declare '${capability}' ` +
          `(required for ${service}.${method})`
      );
    }

    // Stamp who raised a notification. The store trusts `appId` only because
    // this is the one place it gets written, so overwrite whatever the page
    // sent: a page can put any appId it likes in its params, and the centre
    // would then show another app's name and icon over this app's text.
    if (service === "notification" && method === "show") {
      params = { ...params, appId };
    }

    return this._echoRequest(service, method, params, appId);
  }

  _handleSubscribe({ service, eventName }) {
    const bcId = this.browsingContext?.id;
    if (bcId == null) {
      return;
    }
    const key = `${service}:${eventName}`;
    if (!gSubscriptions.has(bcId)) {
      gSubscriptions.set(bcId, new Set());
    }
    gSubscriptions.get(bcId).add(key);
    // Signal delivery: Echo's SignalBus broadcasts unconditionally (see
    // echo/src/signals.rs) rather than requiring per-client subscription
    // registration on the bus itself, so there is nothing further to
    // register with Echo — the filtering this table performs is entirely
    // local. If a future Echo version adds server-side subscriber lists
    // (an EchoSubscribe MessageType already exists in message.rs's enum,
    // unused today), this would be the place to send it.
  }

  // Called by dispatchSignalToSubscribers() (this file, above) when a
  // `org.geckoOS.*` signal arrives that matches a subscribed service:event
  // for this actor's browsing context.
  forwardEventToContent(service, eventName, data) {
    const bcId = this.browsingContext?.id;
    const key = `${service}:${eventName}`;
    if (bcId == null || !gSubscriptions.get(bcId)?.has(key)) {
      return;
    }
    this.sendAsyncMessage("PyxisBridge:Event", { service, eventName, data });
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

  // --- Capability allow-list ---
  //
  // Maps (service, method) -> a capability string from the vocabulary
  // nova-runtime-host-plan.md §3 defines in manifest.webapp's
  // `permissions[]` (e.g. "filesystem:read", "filesystem:write", "power",
  // "notifications"). Returns null/undefined for calls that need no
  // declared capability (e.g. harmless read-only introspection) — err on
  // the side of gating anything that touches the filesystem, power state,
  // or app lifecycle, per the plan's security posture.
  //
  // This table is intentionally small and hand-maintained here rather than
  // derived automatically from gaia-bridge.js's method list, so that adding
  // a new bridge method requires a deliberate, reviewable decision about
  // its capability requirement rather than silently inheriting "no
  // permission needed."
  _capabilityFor(service, method) {
    const table = {
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
        setScreenBrightness: "power",
        setScreenTimeout: "power",
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
      appManager: {
        install: "appManager:install",
        uninstall: "appManager:install",
        kill: "appManager:manage",
        restart: "appManager:manage",
      },
    };
    return table[service]?.[method] ?? null;
  }

  // Resolves an app-id's declared permissions[] from Nova's registry (now a
  // real in-process service inside echo-daemon, see
  // echo/src/services/nova.rs — `org.geckoOS.nova` -> `GetInfo`, per
  // nova-runtime-host-plan.md §5). Cached per app-id for the lifetime of
  // this process to avoid a round-trip on every single bridge call — Nova
  // re-broadcasts `RegistryChanged`/`AppUpdated` signals on install/
  // uninstall/reload, but this actor doesn't currently subscribe to them,
  // so the cache is intentionally short-lived (cleared whenever a lookup
  // errors, e.g. Echo not yet up) rather than permanent. On any failure to
  // reach Nova (not installed, Echo down), fail closed: an empty array
  // denies every capability-gated call rather than silently allowing them —
  // matching the original deliberately-conservative placeholder this
  // replaces.
  async _getDeclaredPermissions(appId) {
    if (gPermissionsCache.has(appId)) {
      return gPermissionsCache.get(appId);
    }
    try {
      const connection = getEchoConnection();
      const info = await connection.callMethod("nova", "GetInfo", { appId });
      const perms = Array.isArray(info?.permissions) ? info.permissions : [];
      gPermissionsCache.set(appId, perms);
      return perms;
    } catch (e) {
      console.error(
        `[PyxisBridge] failed to resolve permissions for '${appId}' from ` +
          `Nova, denying all capability-gated calls: ${e}`
      );
      return [];
    }
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
    return connection.callMethod(service, method, params ?? {});
  }
}
