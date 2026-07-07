/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// SCAFFOLD NOTICE (Pyxis, 2026-07-04):
// This file is written by inspection against pyxis-scheme-and-kiosk-plan.md
// Part A, cross-checked against the REAL pinned source tree (ESR 140.0.5,
// gecko-dev `release` @ c61a6579702f4c73ee99131bd5fa216ec8f7b72c, cloned to
// target/gecko-src/gecko-dev/ this session — see gecko/GECKO_PIN). It has
// NOT been compiled. Findings from reading the real tree that changed this
// scaffold's shape versus the plan doc's generic sketch:
//
//   1. `nsIProtocolHandler` (netwerk/base/nsIProtocolHandler.idl) at this
//      pin has only THREE members: `scheme` (readonly attr), `newChannel`,
//      and `allowPort`. There is NO `NewURI` method on this interface and NO
//      `GetProtocolFlags` method — flags come ONLY from the static
//      registration table (components.conf's `protocol_config.flags`, see
//      PyxisProtocolHandler.cpp / components.conf in this dir) or from
//      `nsIProtocolHandlerWithDynamicFlags::getFlagsForURI` for handlers that
//      need per-URI-varying flags (about:, view-source: — Pyxis doesn't need
//      this, our flags are constant per scheme).
//   2. There is also NO `URI_SANDBOXED` flag in this ESR's flag set (checked
//      the full const list in the IDL) — the plan doc's components.conf
//      sketch listed one; it doesn't exist at this pin and must be dropped.
//      The real security posture (no cross-scheme escalation, no
//      loadable-by-anyone) is achieved simply by NOT setting
//      URI_LOADABLE_BY_ANYONE / URI_LOADABLE_BY_SUBSUMERS / etc., matching
//      how moz-extension: (see ExtensionProtocolHandler, this same
//      directory upstream) achieves the equivalent posture.
//   3. The closest REAL precedent in this tree is `moz-extension:`
//      (netwerk/protocol/res/ExtensionProtocolHandler.{h,cpp} +
//      SubstitutingProtocolHandler.{h,cpp}), registered with exactly
//      URI_STD | URI_IS_LOCAL_RESOURCE | URI_IS_POTENTIALLY_TRUSTWORTHY |
//      URI_HAS_WEB_EXPOSED_ORIGIN — i.e. almost the literal flag set Pyxis
//      wants. It also already solves the parent-brokered-file-read-for-
//      sandboxed-content-process problem via NeckoParent IPDL methods
//      (`NewFD`/`SubstituteRemoteFileChannel`) — that is the real mechanism
//      PyxisChannel should eventually copy, not a bespoke IPC scheme.
//   4. `nsIStandardURL`'s mutator (`nsIStandardURLMutator`, same directory)
//      takes `Init(urlType, defaultPort, spec, originCharset, baseURI)` as
//      ONE call, not a fluent `.SetSpec()`/`.SetScheme()` chain — fixed
//      below and in the .cpp.
//   5. CORRECTION (found via a real live-tested bug, not by inspection —
//      see BUILD_RUNBOOK.md's dated entry): point 5 originally claimed that
//      plain URI_STD handlers get generic nsIStandardURL construction "for
//      free" from NS_NewURI, keyed off the registered components.conf flags.
//      That is FALSE. NS_NewURI (netwerk/base/nsNetUtil.cpp) does not read
//      protocol flags to pick a URI class at all — it dispatches on a
//      hardcoded, scheme-by-scheme if/else chain. URI_STD=0 is a documentary
//      value only (see nsIProtocolHandler.idl); a scheme absent from that
//      chain falls through to the network.url.useDefaultURI branch and gets
//      a `DefaultURI` instance, which does NOT implement nsIStandardURL.
//      That silently broke content-principal creation for every pyxis://
//      load: ContentPrincipal::GenerateOriginNoSuffixFromURI
//      (caps/ContentPrincipal.cpp) do_QueryInterfaces to nsIStandardURL and
//      fails closed otherwise, and BasePrincipal::CreateContentPrincipal
//      (caps/BasePrincipal.cpp) silently maps that failure to a
//      NullPrincipal — which is why window.isSecureContext read false and
//      contentPrincipal.originNoSuffix read "moz-nullprincipal:..." instead
//      of "pyxis://<app-id>" in live testing, even though registration,
//      origin-map resolution, and content serving all worked correctly.
//      Fixed by adding an explicit `if (scheme.EqualsLiteral("pyxis"))` case
//      to NS_NewURI (right after the existing indexeddb/uuid case, which
//      uses the identical nsStandardURL + URLTYPE_AUTHORITY pattern this
//      scheme needs) — this IS a stock-file change (nsNetUtil.cpp is outside
//      netwerk/protocol/pyxis/), mirrored under gecko/patches/pyxis/ per the
//      project's patch-tracking convention despite being a non-pyxis path.
//      No SubstitutingProtocolHandler-style custom NewURI override on this
//      class is needed beyond that: app-id resolution still happens lazily
//      at channel-open time via PyxisOriginMap, as originally designed.
//
// Remaining TODO before this compiles for real: confirm the exact
// `NS_DECL_NSIPROTOCOLHANDLER` macro still generates bindings for exactly
// {GetScheme, NewChannel, AllowPort} at this pin (grep the generated
// xpidl bindings after a first `./mach build` config-only pass), and decide
// whether Pyxis needs `nsIProtocolHandlerWithDynamicFlags` at all (current
// answer: no, our flags are static per PyxisProtocolHandler instance).

#ifndef mozilla_net_PyxisProtocolHandler_h
#define mozilla_net_PyxisProtocolHandler_h

#include "nsIProtocolHandler.h"
#include "nsIURI.h"
#include "nsIChannel.h"
#include "nsILoadInfo.h"
#include "nsWeakReference.h"
#include "nsString.h"

namespace mozilla {
namespace net {

class PyxisOriginMap;

// Implements the "pyxis" scheme's nsIProtocolHandler.
//
// Design (pyxis-scheme-and-kiosk-plan.md Part A):
//  - Scheme: "pyxis". Contract ID:
//      @mozilla.org/network/protocol;1?name=pyxis
//  - URLs are backed by nsIStandardURL (via NS_MutateURI +
//    nsStandardURL::Mutator), NOT a "simple URI" — this is what gives
//    pyxis://<app-id>/<path> a real authority component (host = app-id) and
//    normal relative-URL resolution (./foo.js, <base>, ES module
//    specifiers).
//  - Protocol flags (see components.conf in this directory for the
//    authoritative, build-registered set — this is the ONLY place flags are
//    specified; there is no GetProtocolFlags() override at this ESR):
//      URI_STD                        - authority + relative-URL parsing
//      URI_IS_POTENTIALLY_TRUSTWORTHY - secure context (SW, crypto.subtle)
//      URI_IS_LOCAL_RESOURCE          - local, not network; scheduling/
//                                        security hints
//      URI_HAS_WEB_EXPOSED_ORIGIN     - URI has a real, non-null web-exposed
//                                        origin (matches moz-extension:'s
//                                        registration; needed for the origin
//                                        to be usable the way https:// pages
//                                        expect)
//      URI_LOADABLE_BY_SUBSUMERS      - REQUIRED: nsIProtocolHandler.idl
//                                        mandates exactly one of six
//                                        "loadable by" flags be set, or
//                                        nsContentSecurityManager::
//                                        CheckChannelHasProtocolSecurityFlag
//                                        hard-fails every load with
//                                        NS_ERROR_CONTENT_BLOCKED (found via
//                                        a real failed asyncOpen). This is
//                                        the correct choice among the six —
//                                        it restricts loading to
//                                        privileged/chrome code and
//                                        principals that subsume a pyxis://
//                                        origin (i.e. same-origin pyxis://
//                                        documents), NOT
//                                        URI_IS_WEBEXTENSION_RESOURCE (that
//                                        is moz-extension:'s own flag for
//                                        the same slot).
//    Deliberately NOT set: ORIGIN_IS_FULL_SPEC (origin must be scheme+host
//    only, so storage/SW-scope partitions per app-id, not per full URL) and
//    URI_LOADABLE_BY_ANYONE (ordinary https:// content must not be able to
//    load pyxis:// at all).
//    Note: this ESR has no URI_SANDBOXED flag (it doesn't exist in
//    nsIProtocolHandler.idl at this pin) — the plan doc's mention of it was
//    aspirational/generic; the equivalent posture is achieved purely by
//    which of the "who can load this" flags above are left unset.
//  - Origin mapping (app-id -> absolute webroot) is NOT hardcoded here; it is
//    owned by Nova and consumed read-only via PyxisOriginMap, which reads
//    /run/pyxis/nova/origins.map (see nova-runtime-host-plan.md §4).
class PyxisProtocolHandler final : public nsIProtocolHandler,
                                    public nsSupportsWeakReference {
 public:
  NS_DECL_ISUPPORTS
  NS_DECL_NSIPROTOCOLHANDLER

  PyxisProtocolHandler();

  // Component registration entry point referenced from components.conf's
  // 'type' field is normally a Constructor via NS_GENERIC_FACTORY_CONSTRUCTOR
  // or similar generated glue -- no explicit static Create() is required for
  // components.conf-based registration in modern trees, but keeping a
  // GetSingleton()-style accessor is useful for other in-tree code (the
  // bridge actor's parent side, see PyxisBridgeParent) that may want to
  // consult the same origin map instance rather than re-opening it.
  static already_AddRefed<PyxisProtocolHandler> GetOrCreate();

  // Scheme name, exposed for other in-tree consumers (loadinfo checks, the
  // bridge actor's registration filter, etc.) rather than repeating the
  // string literal "pyxis" everywhere.
  //
  // Declared as a char array, NOT `static constexpr auto kScheme = "pyxis"`
  // -- `auto` deduces a string literal to `const char*` (a pointer), and
  // nsACString's AssignLiteral/EqualsLiteral template overloads only match
  // an actual array type (they deduce the length from `const char (&)[N]`).
  // A pointer silently fails to match any overload at the call site.
  static constexpr char kScheme[] = "pyxis";

 private:
  ~PyxisProtocolHandler();

  // Lazily constructed, ref-counted; NOT static/global-lifetime by itself --
  // owned via the singleton in GetOrCreate() so shutdown ordering is
  // explicit rather than relying on static destructors racing XPCOM
  // shutdown.
  RefPtr<PyxisOriginMap> mOriginMap;

  PyxisOriginMap* OriginMap();
};

}  // namespace net
}  // namespace mozilla

#endif  // mozilla_net_PyxisProtocolHandler_h
