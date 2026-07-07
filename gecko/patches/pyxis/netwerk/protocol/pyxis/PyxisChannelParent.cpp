/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

#include "PyxisChannelParent.h"
#include "PyxisChannelUtils.h"
#include "PyxisOriginMap.h"

#include "nsIInputStream.h"
#include "nsString.h"

namespace mozilla {
namespace net {

mozilla::ipc::IPCResult PyxisChannelParent::RecvGetContent(
    const nsCString& aAppId, const nsCString& aPath,
    GetContentResolver&& aResolve) {
  // Element types here are NOT arbitrary -- they must match exactly what
  // IPDL codegen derives for each `returns(...)` field's resolver tuple (see
  // ipc/ipdl/ipdl/lower.py's _cxxInType, which _resolveType/_makeResolver
  // build the resolver's std::function<void(std::tuple<...>)> signature
  // from). Verified against that logic rather than assumed:
  //   - nsresult / int64_t: plain Cxx types, not refcounted, not "nsCString"/
  //     "nsString" by name -> const T&.
  //   - nsCString: Cxx type whose *name* is "nsCString" gets special-cased to
  //     the abstract-string base -> const nsACString&.
  //   - nsIInputStream: `[RefCounted] using class` (declared in
  //     mozilla/ipc/IPCStreamUtils.h, included via PNecko.ipdl/DOMTypes.ipdlh
  //     elsewhere, redeclared for this file below) with no [MoveOnly]
  //     attribute -> isSendMoveOnly() is false -> falls into the
  //     isCxx()+isRefcounted() branch, which resolves to a bare `T*` (raw
  //     pointer), NOT RefPtr<T>&& -- confirmed directly against a real
  //     generated single-value case in this exact tree,
  //     `typedef std::function<void(nsIInputStream*)>
  //     GetExtensionStreamResolver;` (netwerk/ipc/PNecko.ipdl's
  //     GetExtensionStream), which uses the identical nsIInputStream using
  //     declaration.
  //
  //     CORRECTION (2026-07-05, found via a real failed build): the above
  //     only holds when the .ipdl parameter is declared `nullable`.
  //     PPyxisChannel.ipdl's GetContent originally omitted it, which made
  //     IPDL codegen wrap this element in NotNull<> on both sides instead
  //     (NotNull<nsIInputStream*> here, NotNull<RefPtr<nsIInputStream>> in
  //     PyxisChannelChild.cpp's resolve-value tuple) -- a NotNull<> can't
  //     represent "no stream, this failed", which is exactly what the two
  //     failure-path aResolve() calls below need. Fixed by adding
  //     `nullable` to PPyxisChannel.ipdl's declaration, matching
  //     GetExtensionStream's own `nullable nsIInputStream stream` exactly
  //     -- Type below is correct as originally written, it just didn't
  //     match the .ipdl file until that keyword was added.
  // This mirrors the real multi-value resolver call convention in
  // ipc/glue/BackgroundParentImpl.cpp's RecvEnsureRDDProcessAndCreateBridge:
  // build a named `Type` tuple alias matching the resolver's declared
  // signature, then construct+pass it, rather than std::make_tuple (which
  // would deduce plain value types and not match the declared std::function
  // parameter type).
  using Type = std::tuple<const nsresult&, nsIInputStream*, const nsACString&,
                          const int64_t&>;

  // Named locals for the failure-path placeholder values below -- NOT
  // inlined as literals at the call site. Type's 1st and 4th elements are
  // reference types (const nsresult&, const int64_t&); binding those
  // references directly to a temporary rvalue (e.g. the literal `-1`) does
  // NOT have its lifetime extended to match the constructed Type object (a
  // tuple member-reference is not a direct reference-binding for lifetime-
  // extension purposes), producing a dangling reference the moment the full
  // expression ends -- caught at compile time by libstdc++'s tuple
  // constructor (found via a real failed build, not by inspection). Named
  // locals with automatic storage spanning the aResolve() call sidestep
  // this entirely.
  static constexpr int64_t kNoContentLength = -1;
  const nsCString kEmptyMime;
  // Same reasoning applies to the success path's status argument below --
  // `NS_OK` is an enum prvalue, not a named lvalue, so binding it directly
  // to Type's `const nsresult&` element would be exactly this same
  // dangling-temporary hazard. Named here proactively (not yet hit by an
  // actual build error) rather than waiting for a second round-trip.
  static constexpr nsresult kOk = NS_OK;

  // PyxisOriginMap::GetOrCreate() is safe here specifically because
  // PyxisChannelParent only ever runs in the parent process (all PBackground
  // parent-side actors do -- see ipc/glue/BackgroundParentImpl.h's Alloc
  // overrides, which only ever execute in the process hosting the
  // PBackground thread, i.e. the parent). This is the whole point of this
  // actor: the equivalent call from a content process (which
  // PyxisChannel::OpenContentStream would have made pre-refactor) is exactly
  // the sandboxed-filesystem-access problem this file exists to route
  // around.
  RefPtr<PyxisOriginMap> originMap = PyxisOriginMap::GetOrCreate();

  nsAutoCString resolvedPath;
  nsresult rv = PyxisChannelUtils::ResolveAndConfinePath(
      originMap, aAppId, aPath, resolvedPath);
  if (NS_FAILED(rv)) {
    aResolve(Type(rv, nullptr, kEmptyMime, kNoContentLength));
    return IPC_OK();
  }

  nsCOMPtr<nsIInputStream> stream;
  int64_t contentLength = -1;
  rv = PyxisChannelUtils::OpenConfinedFile(resolvedPath, getter_AddRefs(stream),
                                           &contentLength);
  if (NS_FAILED(rv)) {
    aResolve(Type(rv, nullptr, kEmptyMime, kNoContentLength));
    return IPC_OK();
  }

  nsAutoCString mime;
  PyxisChannelUtils::InferContentType(resolvedPath, mime);

  // stream/mime are kept alive by these locals until aResolve (which, per
  // the IPDL-generated resolver, serializes the tuple synchronously before
  // returning) has finished -- no dangling-reference risk despite the raw
  // nsIInputStream* / nsACString& element types.
  aResolve(Type(kOk, stream.get(), mime, contentLength));
  return IPC_OK();
}

}  // namespace net
}  // namespace mozilla
