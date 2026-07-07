/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// Parent-process side of the PPyxisChannel actor pair. See PPyxisChannel.ipdl
// for the full design rationale (why PBackground, why not PNecko, why not a
// full remoted-channel actor like PGIOChannel) and PyxisChannelChild.h for
// the content-process side.
//
// This is the ONLY new code that touches PyxisOriginMap on behalf of a
// content-process request. It runs exclusively in the parent process (all
// PBackground parent-side actors do, by construction -- see
// ipc/glue/BackgroundParentImpl.h), so calling PyxisOriginMap::GetOrCreate()
// / PyxisChannelUtils::ResolveAndConfinePath / opening the resolved file here
// is exactly as safe as PyxisChannel::OpenContentStream's existing
// parent-process-direct code path -- it's the same operations, just invoked
// from an IPDL Recv handler instead of directly from nsIChannel::AsyncOpen.

#ifndef mozilla_net_PyxisChannelParent_h
#define mozilla_net_PyxisChannelParent_h

#include "mozilla/net/PPyxisChannelParent.h"
#include "nsISupportsImpl.h"

namespace mozilla {
namespace net {

class PyxisOriginMap;

class PyxisChannelParent final : public PPyxisChannelParent {
 public:
  NS_INLINE_DECL_REFCOUNTING(PyxisChannelParent)

  PyxisChannelParent() = default;

  // Does the real PyxisOriginMap::Resolve + path confinement + open, via
  // PyxisChannelUtils (shared with PyxisChannel::OpenContentStream's
  // parent-direct path -- see PyxisChannelUtils.h for why that refactor
  // exists). aResolve is the generated IPDL resolver for GetContent's
  // returns(...) clause; called exactly once, synchronously, before this
  // method returns (there is no real async work here beyond a blocking
  // filesystem call, matching the parent-direct path's own synchronous
  // nsIFile calls).
  mozilla::ipc::IPCResult RecvGetContent(
      const nsCString& aAppId, const nsCString& aPath,
      GetContentResolver&& aResolve);

 private:
  ~PyxisChannelParent() = default;
};

}  // namespace net
}  // namespace mozilla

#endif  // mozilla_net_PyxisChannelParent_h
