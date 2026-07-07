/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// SCAFFOLD NOTICE (Pyxis, 2026-07-05): extracted from PyxisChannel.{h,cpp} so
// the resolve+confine+infer-mime logic has exactly one implementation shared
// by both the parent-process-direct-open path (PyxisChannel::OpenContentStream)
// and the new IPDL parent-brokered path (PyxisChannelParent::RecvGetContent,
// see PPyxisChannel.ipdl). Neither caller should reimplement any part of the
// path-traversal defense — see the "highest-severity bug class" note that
// used to live in PyxisChannel.cpp and now lives on ResolveAndConfinePath
// below.
//
// Everything here is PARENT-PROCESS-ONLY. It touches PyxisOriginMap (which
// itself reads /run/pyxis/nova/origins.map off disk) and opens real files.
// Callers in a content process must never call these directly -- that is
// exactly the sandboxed-filesystem-access problem PPyxisChannel.ipdl exists
// to solve. (There's no runtime assertion of that here because the only two
// callers, both in this directory, already only run this code when they are
// definitely in the parent: PyxisChannel::OpenContentStream is guarded by its
// own XRE_IsContentProcess() branch, and PyxisChannelParent only ever runs in
// the parent by construction -- see PyxisChannelParent.h.)

#ifndef mozilla_net_PyxisChannelUtils_h
#define mozilla_net_PyxisChannelUtils_h

#include "nsString.h"

class nsIInputStream;

namespace mozilla {
namespace net {

class PyxisOriginMap;

// Everything nsBaseChannel-shaped needs to turn a pyxis://<app-id>/<path>
// request into either a confined, realpath'd absolute filesystem path, or a
// proper failure result. Stateless (module of the origin map handed in);
// safe to call from either PyxisChannel (parent-process direct path) or
// PyxisChannelParent (content-process-brokered path).
class PyxisChannelUtils final {
 public:
  // Resolves aAppId + aPath (the pyxis:// URI's host and filePath
  // components, respectively -- see callers for how each obtains them from
  // nsIURI) to a confined, realpath'd absolute filesystem path via
  // aOriginMap, OR returns a failure result if the app-id is unknown or the
  // path escapes the webroot.
  //
  // aPath is expected in the same form nsIURI::GetFilePath returns it
  // (leading '/', no leading-slash-stripping done by the caller) -- this
  // function does that stripping itself, once, since both callers need it.
  static nsresult ResolveAndConfinePath(PyxisOriginMap* aOriginMap,
                                       const nsACString& aAppId,
                                       const nsACString& aPath,
                                       nsCString& aResolvedAbsolutePath);

  // Small explicit extension -> MIME map. Returns "application/octet-stream"
  // for anything not recognized -- never guesses an executable/script type
  // for an unrecognized extension. Pure function, no parent-process
  // filesystem access, but kept alongside ResolveAndConfinePath since both
  // callers need both, in the same order, every time.
  static void InferContentType(const nsACString& aPath, nsACString& aOutMime);

  // Shared "open the resolved file and hand back a blocking input stream +
  // its size" step, used by both callers after ResolveAndConfinePath
  // succeeds. Returns NS_ERROR_FILE_NOT_FOUND for a missing file or a
  // directory (matching the pre-refactor behavior in PyxisChannel.cpp).
  static nsresult OpenConfinedFile(const nsACString& aResolvedAbsolutePath,
                                   nsIInputStream** aStream,
                                   int64_t* aContentLength);

 private:
  PyxisChannelUtils() = delete;
  ~PyxisChannelUtils() = delete;
};

}  // namespace net
}  // namespace mozilla

#endif  // mozilla_net_PyxisChannelUtils_h
