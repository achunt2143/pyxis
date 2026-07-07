/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#include "PyxisChannelChild.h"
#include "PyxisChannel.h"

#include "mozilla/ipc/BackgroundChild.h"
#include "mozilla/ipc/PBackgroundChild.h"
#include "mozilla/Unused.h"
#include "nsIChannel.h"
#include "nsIInputStream.h"
#include "nsIInputStreamPump.h"
#include "nsIStreamListener.h"
#include "nsIURI.h"
#include "nsNetUtil.h"
#include "nsThreadUtils.h"

namespace mozilla {
namespace net {

NS_IMPL_ISUPPORTS(PyxisChannelChild, nsICancelable)

PyxisChannelChild::PyxisChannelChild(nsIStreamListener* aListener,
                                     PyxisChannel* aChannel)
    : mListener(aListener), mChannel(aChannel) {
  MOZ_ASSERT(mListener);
  MOZ_ASSERT(mChannel);
}

nsresult PyxisChannelChild::StartRequest(nsIURI* aURI) {
  NS_ENSURE_STATE(aURI);

  // Same two URI components PyxisChannelUtils::ResolveAndConfinePath expects
  // (app-id = host, path = filePath) -- extracted here rather than in the
  // parent so the parent-side Recv handler's signature stays two plain
  // strings, not an nsIURI (which would need its own IPDL serialization
  // support and isn't needed for anything else on the parent side).
  nsAutoCString appId;
  nsresult rv = aURI->GetAsciiHost(appId);
  NS_ENSURE_SUCCESS(rv, rv);

  nsAutoCString path;
  rv = aURI->GetFilePath(path);
  NS_ENSURE_SUCCESS(rv, rv);

  mozilla::ipc::PBackgroundChild* backgroundChild =
      mozilla::ipc::BackgroundChild::GetOrCreateForCurrentThread();
  if (NS_WARN_IF(!backgroundChild)) {
    return NS_ERROR_FAILURE;
  }

  // AddRef'd here; released in the resolve/reject callbacks below (which are
  // always eventually called exactly once by the IPDL layer for a
  // successfully-sent async message with a `returns(...)` clause) or, if
  // SendPPyxisChannelConstructor itself fails synchronously, immediately
  // below.
  if (!backgroundChild->SendPPyxisChannelConstructor(this)) {
    return NS_ERROR_FAILURE;
  }

  // Single ResolveOrRejectValue callback (rather than separate resolve/
  // reject lambdas) for a tuple-valued returns(...) promise -- matches the
  // real precedent for consuming a multi-value IPDL returns(...) promise in
  // this tree (see dom/media/ipc/RemoteMediaManagerChild.cpp's
  // SendEnsureRDDProcessAndCreateBridge()->Then(...) call site, which
  // resolves the analogous EnsureRDDProcessAndCreateBridgePromise via
  // std::get<N>(aResult.ResolveValue())).
  RefPtr<PyxisChannelChild> self = this;
  SendGetContent(appId, path)
      ->Then(GetCurrentSerialEventTarget(), __func__,
            [self](GetContentPromise::ResolveOrRejectValue&& aResult) {
              if (aResult.IsReject()) {
                // Parent process gone, content process shutting down, etc.
                // -- not a resolve/reject the protocol itself can
                // distinguish further; treat like any other failed load.
                self->FailRequest(NS_ERROR_FAILURE);
                return;
              }
              auto& tuple = aResult.ResolveValue();
              // std::get<1>(tuple) is a plain RefPtr<nsIInputStream> (NOT
              // NotNull-wrapped) now that PPyxisChannel.ipdl's GetContent
              // declares aStream as `nullable` -- matching the real
              // GetExtensionStream precedent's plain RefPtr<nsIInputStream>
              // resolve-value type exactly (see PPyxisChannel.ipdl's
              // correction note). Move it out directly; forget() is safe to
              // call on the rvalue since it isn't ref-qualified.
              self->OnContentReply(std::get<0>(tuple),
                                   std::get<1>(std::move(tuple)).forget(),
                                   std::get<2>(tuple), std::get<3>(tuple));
            });

  return NS_OK;
}

void PyxisChannelChild::OnContentReply(nsresult aStatus,
                                       already_AddRefed<nsIInputStream> aStream,
                                       const nsACString& aMimeType,
                                       int64_t aContentLength) {
  nsCOMPtr<nsIInputStream> stream = std::move(aStream);

  if (mCanceled) {
    FailRequest(mCancelStatus);
    return;
  }

  if (NS_FAILED(aStatus) || !stream) {
    FailRequest(NS_FAILED(aStatus) ? aStatus : NS_ERROR_FAILURE);
    return;
  }

  if (!aMimeType.IsEmpty() && !mChannel->HasContentTypeHint()) {
    // Only set if the channel doesn't already have an externally-supplied
    // content type hint -- HasContentTypeHint() is the exact same
    // nsBaseChannel guard PyxisChannel::OpenContentStream uses for the
    // parent-direct path (see PyxisChannel.cpp); available here because
    // mChannel is typed as the concrete PyxisChannel, not nsIChannel (see
    // PyxisChannelChild.h's constructor comment).
    mChannel->SetContentType(aMimeType);
  }
  if (aContentLength >= 0) {
    mChannel->SetContentLength(aContentLength);
  }

  nsCOMPtr<nsIInputStreamPump> pump;
  nsresult rv = NS_NewInputStreamPump(getter_AddRefs(pump), stream.forget(), 0,
                                      0, false, GetCurrentSerialEventTarget());
  if (NS_FAILED(rv)) {
    FailRequest(rv);
    return;
  }

  rv = pump->AsyncRead(mListener);
  if (NS_FAILED(rv)) {
    FailRequest(rv);
    return;
  }

  mPump = pump;
}

void PyxisChannelChild::FailRequest(nsresult aResult) {
  RefPtr<PyxisChannel> channel = std::move(mChannel);
  nsCOMPtr<nsIStreamListener> listener = std::move(mListener);
  if (!channel || !listener) {
    // Already failed/completed once -- BeginAsyncRead's contract is one
    // Start/Stop pair per request; guard against a double-call (e.g. Cancel
    // racing the reply).
    return;
  }

  // channel is an lvalue RefPtr<PyxisChannel>; nsCOMPtr's implicit
  // RefPtr<U>&& constructor only accepts an rvalue, and channel isn't
  // used again after this line, so transfer ownership via forget()
  // rather than taking an extra AddRef/Release pair via .get().
  nsCOMPtr<nsIChannel> asChannel = channel.forget();
  listener->OnStartRequest(asChannel);
  listener->OnStopRequest(asChannel, aResult);
  asChannel->CancelWithReason(NS_FAILED(aResult) ? aResult : NS_ERROR_FAILURE,
                              "PyxisChannelChild::FailRequest"_ns);
}

NS_IMETHODIMP
PyxisChannelChild::Cancel(nsresult aReason) {
  if (mCanceled) {
    return NS_OK;
  }
  mCanceled = true;
  mCancelStatus = NS_FAILED(aReason) ? aReason : NS_ERROR_ABORT;

  if (mPump) {
    mPump->CancelWithReason(mCancelStatus, "PyxisChannelChild::Cancel"_ns);
    mPump = nullptr;
    return NS_OK;
  }

  // No pump yet (still waiting on the parent's reply, or the reply already
  // came back as a failure and mChannel/mListener are already null) -- if
  // there's still a channel/listener to notify, OnContentReply/FailRequest
  // above will see mCanceled and fail the request itself once the in-flight
  // IPDL promise settles.
  return NS_OK;
}

}  // namespace net
}  // namespace mozilla
