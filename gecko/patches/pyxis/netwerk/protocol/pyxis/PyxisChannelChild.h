/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Content-process side of the PPyxisChannel actor pair (see
// PPyxisChannel.ipdl for the full design rationale). Named
// PyxisChannelChild/PyxisChannelParent, mirroring GIOChannelChild/
// GIOChannelParent's naming convention in netwerk/protocol/gio/ -- the
// closest same-directory-shaped precedent -- even though (per the .ipdl's
// design note) the actual manager/lifecycle model here is closer to
// dom/locks' PBackground-managed actors than to PGIOChannel's PNecko-managed
// remoted-nsIChildChannel model.
//
// PyxisChannelChild is deliberately NOT itself an nsIChannel/nsBaseChannel --
// unlike GIOChannelChild, which doubles as both the IPDL actor AND the
// content-process nsIChildChannel. PyxisChannel (PyxisChannel.h/.cpp) stays
// the one and only nsIChannel implementation for pyxis:// in both processes;
// PyxisChannelChild is purely a short-lived helper object that PyxisChannel::
// BeginAsyncRead creates, uses to make one GetContent request, and drops once
// the reply (or a connection error) has been delivered to the channel's
// listener. This mirrors ExtensionStreamGetter's role in
// netwerk/protocol/res/ExtensionProtocolHandler.cpp exactly -- that class
// isn't a channel either, it's a helper nsICancelable that owns exactly one
// in-flight parent request and feeds the result to a listener + pump.

#ifndef mozilla_net_PyxisChannelChild_h
#define mozilla_net_PyxisChannelChild_h

#include "mozilla/net/PPyxisChannelChild.h"
#include "mozilla/RefPtr.h"
#include "nsICancelable.h"
#include "nsCOMPtr.h"
#include "nsString.h"

class nsIStreamListener;
class nsIInputStreamPump;
class nsIURI;
class nsIInputStream;

namespace mozilla {
namespace net {

class PyxisChannel;

// One instance per content-process pyxis:// load. Constructed and sent by
// PyxisChannel::BeginAsyncRead (see PyxisChannel.cpp), which is also the
// only holder of a strong reference via the nsICancelable it hands back to
// nsBaseChannel's BeginPumpingData -- see nsBaseChannel.h's BeginAsyncRead
// doc comment on why a cancelable-in-lieu-of-a-request is the correct thing
// to return here (the request literally doesn't exist yet; it's still an
// in-flight IPDL round trip to the parent when BeginAsyncRead returns).
class PyxisChannelChild final : public PPyxisChannelChild, public nsICancelable {
 public:
  NS_DECL_ISUPPORTS
  NS_DECL_NSICANCELABLE

  // aChannel is always the PyxisChannel instance that owns this request
  // (constructed by PyxisChannel::BeginAsyncRead passing `this`) -- typed as
  // the concrete class, not nsIChannel, so this class can call
  // HasContentTypeHint()/SetContentType()/SetContentLength(), which are
  // nsBaseChannel members, not part of the nsIChannel XPCOM interface (see
  // PyxisChannel::OpenContentStream's own use of HasContentTypeHint() for
  // the parent-direct path -- this mirrors it exactly for the brokered
  // path).
  PyxisChannelChild(nsIStreamListener* aListener, PyxisChannel* aChannel);

  // Extracts app-id/path from aURI (same components
  // PyxisChannelUtils::ResolveAndConfinePath expects -- see that header),
  // gets (or creates) the current thread's PBackgroundChild actor, sends
  // this actor's constructor over it, and finally sends GetContent. Called
  // once, synchronously, from PyxisChannel::BeginAsyncRead.
  nsresult StartRequest(nsIURI* aURI);

 private:
  ~PyxisChannelChild() = default;

  // IPDL reply handler for SendGetContent's promise. Builds an
  // nsInputStreamPump over the returned stream (on success) or synthesizes
  // an OnStartRequest/OnStopRequest(failure) pair (on failure/cancel) and
  // notifies mListener either way -- mirrors ExtensionStreamGetter::OnStream
  // in netwerk/protocol/res/ExtensionProtocolHandler.cpp.
  void OnContentReply(nsresult aStatus, already_AddRefed<nsIInputStream> aStream,
                      const nsACString& aMimeType, int64_t aContentLength);

  // Common failure path for OnContentReply's failure branch AND Cancel():
  // synthesizes OnStartRequest+OnStopRequest(aResult) on mListener and drops
  // our references, matching ExtensionStreamGetter::CancelRequest exactly
  // (same reasoning: nsBaseChannel's contract requires a matched Start/Stop
  // pair even for a request that never really started).
  void FailRequest(nsresult aResult);

  nsCOMPtr<nsIStreamListener> mListener;
  RefPtr<PyxisChannel> mChannel;
  nsCOMPtr<nsIInputStreamPump> mPump;
  bool mCanceled = false;
  nsresult mCancelStatus = NS_OK;
};

}  // namespace net
}  // namespace mozilla

#endif  // mozilla_net_PyxisChannelChild_h
