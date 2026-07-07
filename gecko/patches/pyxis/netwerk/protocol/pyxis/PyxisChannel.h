/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// SCAFFOLD NOTICE (Pyxis, 2026-07-04): see PyxisProtocolHandler.h for the
// full disclaimer and ground-truth findings against the pinned ESR 140.0.5
// tree (target/gecko-src/gecko-dev/, gecko-dev `release` @
// c61a6579702f4c73ee99131bd5fa216ec8f7b72c). Key finding that reshaped this
// file: hand-rolling nsIChannel from scratch (as a naive reading of the plan
// doc's "implement nsIChannel" line might suggest) is unnecessary and would
// duplicate a lot of real machinery. netwerk/base/nsBaseChannel.h is the
// documented, widely-used base class for exactly this shape of channel
// ("stream bytes for a URI") — subclasses implement ONE method,
// OpenContentStream(bool async, nsIInputStream** stream, nsIChannel**
// channel). This is confirmed real and current at the pin (still present,
// still documented as the intended subclassing point). PyxisChannel is
// written against nsBaseChannel below instead of raw nsIChannel.
//
// UPDATE (Pyxis, 2026-07-05): the sandboxed-content-process gap flagged
// throughout this file's comments is now closed -- see PPyxisChannel.ipdl
// for the full design and PyxisChannelChild.h/PyxisChannelParent.h for the
// new actor pair. The fix is NOT simply a branch inside OpenContentStream
// (the originally-sketched shape): nsBaseChannel::BeginPumpingData (see
// netwerk/base/nsBaseChannel.cpp) tries BeginAsyncRead() FIRST and only
// falls back to OpenContentStream() if BeginAsyncRead() returns
// NS_ERROR_NOT_IMPLEMENTED. BeginAsyncRead is the override actually meant
// for "the stream isn't available synchronously, async work is needed to
// get it" (see nsBaseChannel.h's own doc comment on BeginAsyncRead, and
// SimpleChannel::BeginAsyncRead/ExtensionStreamGetter for the real
// moz-extension: precedent using exactly this override for the same
// content-process-proxy purpose). So: OpenContentStream keeps doing the
// direct, parent-process-only open (now via the shared PyxisChannelUtils
// helper), and a NEW BeginAsyncRead override below handles the
// content-process path by delegating to PyxisChannelChild. XRE_IsContentProcess()
// still gates which one actually does the work in this build's process
// model, since that check now lives in BeginAsyncRead (return
// NS_ERROR_NOT_IMPLEMENTED there when NOT a content process, so the
// existing OpenContentStream path runs unchanged in the parent).

#ifndef mozilla_net_PyxisChannel_h
#define mozilla_net_PyxisChannel_h

#include "nsBaseChannel.h"
#include "nsCOMPtr.h"
#include "nsIURI.h"
#include "nsILoadInfo.h"

namespace mozilla {
namespace net {

class PyxisOriginMap;

// nsIChannel implementation for pyxis://<app-id>/<path>, subclassing
// nsBaseChannel (see notice above) rather than nsIChannel directly.
//
// Responsibilities (pyxis-scheme-and-kiosk-plan.md §A4):
//  1. Resolve app-id (aURI's host/authority) -> webroot via PyxisOriginMap.
//     Unknown app-id -> NS_ERROR_FILE_NOT_FOUND from OpenContentStream (a
//     proper load error, not a crash/abort).
//  2. Canonicalize + confine the requested path to that webroot. This is the
//     highest-severity bug class per the plan's risk register (Part D) —
//     realpath() the resolved path and assert it is still prefixed by the
//     webroot's realpath before opening anything; reject any `..`
//     component, any symlink that escapes the webroot, and any
//     absolute-path re-root attempt in the URI's path component.
//  3. Infer Content-Type from a small explicit extension allowlist
//     (.html/.js/.mjs/.css/.wasm/.json/images/fonts), defaulting to
//     application/octet-stream. Never sniff/execute-type based on content.
//  4. Set a strict default CSP for app documents (see SetupResponseHeaders
//     below).
//  5. Stream asynchronously — nsBaseChannel's OpenContentStream(async=true)
//     path uses nsInputStreamPump internally when we hand back a plain
//     nsIInputStream, so a straightforward nsIFile -> NS_NewLocalFileInputStream
//     (wrapped so reads happen off the main thread; nsBaseChannel's pump
//     machinery handles this for a blocking stream — see the "blocking vs
//     non-blocking" note in nsBaseChannel.h's OpenContentStream doc comment)
//     satisfies this without hand-written async plumbing.
//
// THE SANDBOXED-CONTENT-PROCESS PROBLEM (§A4's "subtle part") — RESOLVED
// (2026-07-05): the above works correctly when PyxisChannel::OpenContentStream
// runs in the PARENT process. When a content-process document loads a
// pyxis:// sub-resource, NewChannel (PyxisProtocolHandler.cpp) is invoked IN
// THE CONTENT PROCESS, and this class must NOT naively open() the file there
// — the content sandbox forbids arbitrary filesystem reads. The real,
// working precedent in this exact tree is moz-extension:'s
// ExtensionProtocolHandler::SubstituteRemoteFileChannel / NewFD, which
// detect "we're in a content process" and swap in a channel that proxies the
// read to the parent over a NeckoParent IPDL method, getting back an
// IPCStream/FD. PyxisChannel now has the equivalent: a small,
// Pyxis-dedicated `manager PBackground` protocol (PPyxisChannel.ipdl,
// PyxisChannelChild/PyxisChannelParent) rather than growing shared
// NeckoParent/NeckoChild — see PPyxisChannel.ipdl's design note for the full
// reasoning on why PBackground was chosen over PNecko/a full remoted-channel
// actor like PGIOChannel. BeginAsyncRead() below is where the content-process
// branch actually lives (NOT OpenContentStream — see the UPDATE note at the
// top of this file for why nsBaseChannel's own dispatch order made that the
// correct override point once the real build tree existed to check against).
// Constructor shape matches nsFileChannel's real pattern in this tree
// (netwerk/protocol/file/nsFileChannel.h): take just the URI and call
// SetURI() inside Init(); nsIChannel's `loadInfo` is a separate settable
// attribute (forwarded from nsBaseChannel via NS_FORWARD_NSICHANNEL in this
// class's NS_DECL block below), set by the CALLER
// (PyxisProtocolHandler::NewChannel) after construction — NOT threaded
// through this constructor. aOriginMap is still taken here (not part of
// nsIChannel's surface at all) since PyxisChannel, uniquely to this scheme,
// needs it to resolve app-id -> webroot in OpenContentStream().
class PyxisChannel final : public nsBaseChannel {
 public:
  NS_DECL_ISUPPORTS_INHERITED
  NS_FORWARD_NSIREQUEST(nsBaseChannel::)
  NS_FORWARD_NSICHANNEL(nsBaseChannel::)

  explicit PyxisChannel(nsIURI* aURI, PyxisOriginMap* aOriginMap);

  // Called by PyxisProtocolHandler::NewChannel right after construction,
  // matching nsFileChannel's Init()/SetURI() split.
  nsresult Init();

  // NOTE: HasContentTypeHint()/SetContentType()/SetContentLength() are
  // already public nsBaseChannel members (see nsBaseChannel.h's "Methods
  // provided for use by the derived class" section) inherited here with no
  // extra declaration needed. PyxisChannelChild.cpp's OnContentReply calls
  // all three directly through a PyxisChannel* for the content-process-
  // brokered path, exactly mirroring what OpenContentStream below does for
  // the parent-direct path -- this is the reason PyxisChannelChild stores
  // its channel as a RefPtr<PyxisChannel> rather than nsCOMPtr<nsIChannel>
  // (see PyxisChannelChild.h's constructor comment).

 private:
  ~PyxisChannel();

  // nsBaseChannel's one required override. Parent-process-only in practice
  // now: BeginAsyncRead() below intercepts the content-process case first
  // (nsBaseChannel::BeginPumpingData tries BeginAsyncRead before falling
  // back to this), so by the time this runs we're guaranteed to be in the
  // parent (either because we truly are the parent process, or because a
  // synchronous, non-async Open() call was made -- see the assertion in the
  // .cpp for why that combination is intentionally still treated as a
  // direct, unbrokered open rather than also being routed through IPDL: a
  // synchronous nsIChannel::Open() has no async continuation to route a
  // parent round-trip through in the first place, and no in-tree caller of
  // pyxis:// uses Open() over AsyncOpen()).
  nsresult OpenContentStream(bool aAsync, nsIInputStream** aStream,
                              nsIChannel** aChannel) override;

  // The content-process-safe entry point (see the class's big "SANDBOXED-
  // CONTENT-PROCESS PROBLEM" notice above for why this exists and why it's
  // BeginAsyncRead rather than a branch inside OpenContentStream). Returns
  // NS_ERROR_NOT_IMPLEMENTED when NOT running in a content process, which
  // per nsBaseChannel::BeginPumpingData's contract falls through to
  // OpenContentStream unchanged -- i.e. parent-process behavior is
  // byte-for-byte identical to before this method existed. When running in
  // a content process, constructs a PyxisChannelChild, kicks off the
  // GetContent IPDL request, and returns it as the outparam cancelable (see
  // PyxisChannelChild.h/.cpp).
  nsresult BeginAsyncRead(nsIStreamListener* aListener, nsIRequest** aRequest,
                          nsICancelable** aCancelableRequest) override;

  // Resolves this channel's URI (host = app-id, filePath = path) to a
  // confined, realpath'd absolute filesystem path via mOriginMap, OR returns
  // a failure result if the app-id is unknown or the path escapes the
  // webroot. Thin wrapper around PyxisChannelUtils::ResolveAndConfinePath
  // (see PyxisChannelUtils.h) that supplies this channel's URI/mOriginMap --
  // kept as a member so OpenContentStream's call site doesn't change, and
  // so the traversal-defense gtest battery (§A6) can still exercise it
  // through a real PyxisChannel instance if that's more convenient than
  // calling PyxisChannelUtils directly.
  nsresult ResolveAndConfinePath(nsCString& aResolvedAbsolutePath);

  // Small explicit extension -> MIME map (class comment point 3). Thin
  // wrapper around PyxisChannelUtils::InferContentType -- see that header
  // for the real implementation, shared with PyxisChannelParent's
  // content-process-brokered path.
  static void InferContentType(const nsACString& aPath, nsACString& aOutMime);

  // Sets Content-Length, a conservative caching policy, and the default
  // per-app-document CSP (pyxis-scheme-and-kiosk-plan.md §A4's "strict
  // default CSP for app documents") on the channel's response head. Exact
  // header-setting API depends on nsBaseChannel's response-info hooks at
  // this pin (grep nsBaseChannel.h for SetContentLength / mResponseHead
  // access — this class currently stubs the call site; see .cpp TODO).
  void SetupResponseHeaders(int64_t aContentLength, const nsACString& aMime);

  // NOTE: nsBaseChannel already owns mURI/mOriginalURI (via SetURI()/URI())
  // and the loadInfo (via the forwarded nsIChannel implementation) — no need
  // to duplicate those as members here, unlike the earlier draft of this
  // file.
  RefPtr<PyxisOriginMap> mOriginMap;
};

}  // namespace net
}  // namespace mozilla

#endif  // mozilla_net_PyxisChannel_h
