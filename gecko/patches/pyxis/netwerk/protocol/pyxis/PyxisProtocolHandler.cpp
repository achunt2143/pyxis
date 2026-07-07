/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// SCAFFOLD NOTICE: see PyxisProtocolHandler.h for the full ground-truth
// findings from reading the real pinned tree (ESR 140.0.5). Summary of what
// changed here versus a naive reading of pyxis-scheme-and-kiosk-plan.md:
//   - nsIProtocolHandler at this pin has exactly three members: `scheme`
//     (attribute), `newChannel(uri, loadinfo)`, `allowPort(port, scheme)`.
//     No NewURI, no GetProtocolFlags, no GetDefaultPort on the interface —
//     confirmed against netwerk/base/nsIProtocolHandler.idl in the cloned
//     tree. All of "flags" and "default port" are registration-table-only
//     concepts (components.conf's protocol_config), not virtual methods to
//     override.
//   - Because Pyxis doesn't need SubstitutingProtocolHandler-style
//     construction-time resolution logic (see header comment point 5), no
//     custom NewURI is implemented at all — the netwerk core builds the
//     nsIStandardURL generically from the URI_STD registration, exactly as
//     it does for plain http/https-shaped registrations.
//   - This is NOT yet wired for the sandboxed-content-process file-read
//     problem (pyxis-scheme-and-kiosk-plan.md §A4, the single highest-risk
//     item per that doc's Part D risk register). The real, working pattern
//     for that in this exact tree is moz-extension:'s
//     ExtensionProtocolHandler::SubstituteRemoteFileChannel /
//     NewFD, which proxy through NeckoParent IPDL methods. PyxisChannel (see
//     PyxisChannel.h/.cpp in this directory) currently documents that
//     requirement but does not yet implement the IPDL leg — that is real,
//     non-trivial follow-up work requiring new NeckoParent-style IPDL
//     messages (or reuse of the existing RemoteFileStream machinery
//     moz-extension: uses), not something to fake with a TODO comment
//     inside PyxisChannel alone.

#include "PyxisProtocolHandler.h"
#include "PyxisChannel.h"
#include "PyxisOriginMap.h"

#include "nsNetCID.h"
#include "mozilla/StaticPtr.h"
#include "mozilla/ClearOnShutdown.h"

namespace mozilla {
namespace net {

namespace {
// Named uniquely (not just "sSingleton") because Gecko's unified build
// concatenates multiple .cpp files into one translation unit -- an
// anonymous namespace no longer guarantees per-file isolation once that
// happens, and a same-named anonymous-namespace global in another file in
// the same unified unit (PyxisOriginMap.cpp) would otherwise redefine this
// one.
StaticRefPtr<PyxisProtocolHandler> sProtocolHandlerSingleton;
}  // namespace

NS_IMPL_ISUPPORTS(PyxisProtocolHandler, nsIProtocolHandler,
                  nsISupportsWeakReference)

PyxisProtocolHandler::PyxisProtocolHandler() = default;
PyxisProtocolHandler::~PyxisProtocolHandler() = default;

/* static */
already_AddRefed<PyxisProtocolHandler> PyxisProtocolHandler::GetOrCreate() {
  if (!sProtocolHandlerSingleton) {
    sProtocolHandlerSingleton = new PyxisProtocolHandler();
    // Ties the singleton's lifetime to XPCOM shutdown rather than a static
    // destructor, matching the pattern used by other in-tree protocol
    // handlers with process-lifetime caches (e.g. ExtensionProtocolHandler's
    // own sSingleton in netwerk/protocol/res/).
    ClearOnShutdown(&sProtocolHandlerSingleton);
  }
  RefPtr<PyxisProtocolHandler> handler = sProtocolHandlerSingleton;
  return handler.forget();
}

PyxisOriginMap* PyxisProtocolHandler::OriginMap() {
  if (!mOriginMap) {
    // PyxisOriginMap::GetOrCreate() owns the (eventually inotify-refreshed)
    // cache of /run/pyxis/nova/origins.map (see PyxisOriginMap.h). Handler
    // instances (there should really only ever be the one singleton, but
    // don't assume that) share the process-wide map rather than each opening
    // their own fd/watch.
    mOriginMap = PyxisOriginMap::GetOrCreate();
  }
  return mOriginMap;
}

// --- nsIProtocolHandler (exactly 3 members at this pin; see header) ---

NS_IMETHODIMP
PyxisProtocolHandler::GetScheme(nsACString& aScheme) {
  aScheme.AssignLiteral(kScheme);
  return NS_OK;
}

NS_IMETHODIMP
PyxisProtocolHandler::AllowPort(int32_t aPort, const char* aScheme,
                                 bool* aAllowed) {
  // pyxis:// never carries a port (default_port: -1 in components.conf);
  // nothing to allow.
  *aAllowed = false;
  return NS_OK;
}

NS_IMETHODIMP
PyxisProtocolHandler::NewChannel(nsIURI* aURI, nsILoadInfo* aLoadInfo,
                                  nsIChannel** aResult) {
  NS_ENSURE_ARG_POINTER(aURI);
  NS_ENSURE_ARG_POINTER(aLoadInfo);

  // Confirm this is actually one of ours (defensive; nsIOService should only
  // ever route pyxis: URIs here, but a bad caller could hand us anything
  // implementing nsIURI with an unexpected scheme).
  nsAutoCString scheme;
  nsresult rv = aURI->GetScheme(scheme);
  NS_ENSURE_SUCCESS(rv, rv);
  if (!scheme.EqualsLiteral(kScheme)) {
    return NS_ERROR_UNKNOWN_PROTOCOL;
  }

  // PyxisChannel resolves app-id (URI host/authority) -> webroot via
  // OriginMap(), canonicalizes + confines the requested path (path-traversal
  // defense, see PyxisChannel.cpp), and is *intended* to arrange for the
  // actual file read to happen in the parent process regardless of which
  // process NewChannel was called in (see PyxisChannel.h's IPC-brokering
  // notes, the moz-extension: precedent cited above, and
  // pyxis-scheme-and-kiosk-plan.md §A4) — the IPDL leg of that is not yet
  // implemented; see PyxisChannel.cpp's own notice.
  //
  // Constructor/Init/SetLoadInfo split matches nsFileChannel's real pattern
  // in this tree (netwerk/protocol/file/nsFileChannel.cpp's
  // NS_NewFileChannelInternal): construct with just the URI, call Init(),
  // then set loadInfo via the (nsBaseChannel-forwarded) nsIChannel setter —
  // loadInfo is NOT a constructor parameter.
  RefPtr<PyxisChannel> channel = new PyxisChannel(aURI, OriginMap());
  rv = channel->Init();
  NS_ENSURE_SUCCESS(rv, rv);
  rv = channel->SetLoadInfo(aLoadInfo);
  NS_ENSURE_SUCCESS(rv, rv);

  channel.forget(aResult);
  return NS_OK;
}

}  // namespace net
}  // namespace mozilla
