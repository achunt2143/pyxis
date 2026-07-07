/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// SCAFFOLD NOTICE: see PyxisChannel.h / PyxisProtocolHandler.h for ground
// truth findings against the pinned ESR 140.0.5 tree. This implementation
// has NOT been compiled. The traversal-defense logic (now in
// PyxisChannelUtils::ResolveAndConfinePath, see that file) is written to be
// genuinely correct by inspection (it's the highest-severity bug class per
// the plan's risk register) but must still pass the real gtest battery
// (§A6) once buildable.
//
// UPDATE (2026-07-05): the sandboxed-content-process file read (see the
// large comment in PyxisChannel.h) is now handled -- BeginAsyncRead below
// routes content-process requests through PyxisChannelChild/
// PPyxisChannel.ipdl instead of touching the filesystem directly.
// OpenContentStream's direct NS_NewLocalFileInputStream-based open (now via
// PyxisChannelUtils::OpenConfinedFile) remains exactly as before and is only
// ever reached in the parent process (or for a synchronous, non-async
// Open() call -- see BeginAsyncRead's own comment on why that combination
// isn't also routed through IPDL).

#include "PyxisChannel.h"
#include "PyxisChannelChild.h"
#include "PyxisChannelUtils.h"
#include "PyxisOriginMap.h"

#include "mozilla/Assertions.h"
#include "mozilla/net/NeckoChannelParams.h"
#include "nsIFile.h"
#include "nsNetUtil.h"
#include "nsProxyRelease.h"
#include "nsStandardURL.h"
#include "nsIURIMutator.h"
#include "nsXULAppAPI.h"

namespace mozilla {
namespace net {

NS_IMPL_ISUPPORTS_INHERITED0(PyxisChannel, nsBaseChannel)

PyxisChannel::PyxisChannel(nsIURI* aURI, PyxisOriginMap* aOriginMap)
    : mOriginMap(aOriginMap) {
  SetURI(aURI);
}

PyxisChannel::~PyxisChannel() = default;

nsresult PyxisChannel::Init() {
  // Placeholder for any pre-flight validation that should happen at
  // construction rather than at open time (e.g. rejecting an obviously
  // malformed host/app-id string). Currently a no-op; ResolveAndConfinePath
  // does the real validation lazily in OpenContentStream, which is
  // sufficient since nsBaseChannel channels are single-use (one
  // AsyncOpen/Open per instance) and OpenContentStream always runs before
  // any bytes are returned.
  return NS_OK;
}

nsresult PyxisChannel::ResolveAndConfinePath(nsCString& aResolvedAbsolutePath) {
  nsIURI* uri = URI();
  NS_ENSURE_STATE(uri);

  // app-id is the URI's host/authority component (pyxis://<app-id>/<path>),
  // per the plan's URI_STD + nsIStandardURL design (see
  // PyxisProtocolHandler.h) — origin is scheme+host, exactly the app-id.
  nsAutoCString appId;
  nsresult rv = uri->GetAsciiHost(appId);
  NS_ENSURE_SUCCESS(rv, rv);

  nsAutoCString path;
  rv = uri->GetFilePath(path);
  NS_ENSURE_SUCCESS(rv, rv);

  // All the actual resolve+confine logic (path-traversal defense included)
  // now lives in PyxisChannelUtils, shared verbatim with
  // PyxisChannelParent::RecvGetContent's brokered-open path -- see
  // PyxisChannelUtils.h for why that refactor exists. This method is now
  // just "extract the two URI components this channel's mOriginMap-based
  // parent-direct path needs, then delegate."
  return PyxisChannelUtils::ResolveAndConfinePath(mOriginMap, appId, path,
                                                  aResolvedAbsolutePath);
}

/* static */
void PyxisChannel::InferContentType(const nsACString& aPath,
                                    nsACString& aOutMime) {
  // Delegates to the shared implementation (see PyxisChannelUtils.h) so
  // PyxisChannelParent::RecvGetContent infers the exact same MIME type for
  // the content-process-brokered path as this class does for the
  // parent-direct path. This wrapper is kept (rather than having every
  // caller in this file switch to PyxisChannelUtils::InferContentType
  // directly) purely so external callers/tests referencing
  // PyxisChannel::InferContentType don't need to change.
  PyxisChannelUtils::InferContentType(aPath, aOutMime);
}

void PyxisChannel::SetupResponseHeaders(int64_t aContentLength,
                                        const nsACString& aMime) {
  // TODO(pyxis): wire Content-Length / caching headers / default CSP once
  // this channel is exercised through a real build — nsBaseChannel exposes
  // content type/length via SetContentType()/SetContentLength() (inherited
  // nsIChannel setters), which OpenContentStream below already calls
  // directly for the common case. A per-app-document default CSP (plan
  // §A4: "a strict default CSP for app documents") is better applied via
  // the response's Content-Security-Policy semantics through
  // nsIHttpChannel-style header injection OR, more likely for a
  // non-nsIHttpChannel local scheme, via the CSP-over-meta/document-policy
  // path content documents already support — decide the exact injection
  // point once a real page loads over pyxis:// and CSP enforcement can be
  // observed/tested (§A6 mochitest acceptance criteria), rather than
  // guessing the mechanism here.
}

nsresult PyxisChannel::OpenContentStream(bool aAsync, nsIInputStream** aStream,
                                         nsIChannel** aChannel) {
  // By the time this runs, BeginAsyncRead() below has already had first
  // refusal (nsBaseChannel::BeginPumpingData tries BeginAsyncRead() before
  // ever falling back here -- see the UPDATE note in PyxisChannel.h). So in
  // the common AsyncOpen() case, reaching this method at all already implies
  // BeginAsyncRead() returned NS_ERROR_NOT_IMPLEMENTED, which only happens
  // when !XRE_IsContentProcess(). The one other way to reach this method is
  // a direct, synchronous nsIChannel::Open() call (async == false), which
  // nsBaseChannel routes straight here regardless of process type. No
  // in-tree caller of pyxis:// uses Open() (only AsyncOpen(), via normal
  // document/subresource loads) -- assert that outright rather than merely
  // assuming it, since a synchronous Open() from a content process would
  // hit this same unsafe-in-content-process direct-open code with no
  // BeginAsyncRead() involved at all.
  MOZ_DIAGNOSTIC_ASSERT(!XRE_IsContentProcess(),
                        "PyxisChannel::OpenContentStream must never run in "
                        "a content process (sandboxed filesystem access) -- "
                        "see PyxisChannel.h's SANDBOXED-CONTENT-PROCESS "
                        "notice");

  nsAutoCString absolutePath;
  nsresult rv = ResolveAndConfinePath(absolutePath);
  if (NS_FAILED(rv)) {
    return rv;
  }

  nsCOMPtr<nsIInputStream> fileStream;
  int64_t size = -1;
  rv = PyxisChannelUtils::OpenConfinedFile(absolutePath,
                                           getter_AddRefs(fileStream), &size);
  if (NS_FAILED(rv)) {
    return rv;
  }

  nsAutoCString mime;
  if (!HasContentTypeHint()) {
    InferContentType(absolutePath, mime);
    SetContentType(mime);
  }
  SetupResponseHeaders(size, mime);

  *aChannel = nullptr;
  fileStream.forget(aStream);
  return NS_OK;
}

nsresult PyxisChannel::BeginAsyncRead(nsIStreamListener* aListener,
                                      nsIRequest** aRequest,
                                      nsICancelable** aCancelableRequest) {
  // See PyxisChannel.h's SANDBOXED-CONTENT-PROCESS notice and
  // PPyxisChannel.ipdl's design note for the full reasoning behind this
  // override existing at all and why it's this method rather than a branch
  // inside OpenContentStream.
  if (!XRE_IsContentProcess()) {
    // Parent process (the common case for most of this scheme's real
    // traffic today, and the ONLY case prior to this change): fall back to
    // OpenContentStream unchanged, exactly as nsBaseChannel::
    // BeginPumpingData's own contract for NS_ERROR_NOT_IMPLEMENTED
    // specifies. Nothing about the parent-process behavior changes here.
    return NS_ERROR_NOT_IMPLEMENTED;
  }

  // Content process: no PyxisOriginMap access, no filesystem access -- ask
  // the parent over PPyxisChannel instead. PyxisChannelChild is a
  // short-lived nsICancelable helper (see PyxisChannelChild.h/.cpp) that
  // owns the in-flight GetContent request and feeds the eventual stream (or
  // failure) to aListener itself; it is NOT an nsIRequest, so it's returned
  // via aCancelableRequest, matching nsBaseChannel::BeginAsyncRead's own
  // documented contract ("a cancelable object... when a request can't be
  // returned right away due to some async work needed to retrieve it").
  RefPtr<PyxisChannelChild> childActor = new PyxisChannelChild(aListener, this);
  nsresult rv = childActor->StartRequest(URI());
  if (NS_FAILED(rv)) {
    return rv;
  }

  childActor.forget(aCancelableRequest);
  return NS_OK;
}

}  // namespace net
}  // namespace mozilla
