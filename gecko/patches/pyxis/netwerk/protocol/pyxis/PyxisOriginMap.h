/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// SCAFFOLD NOTICE: see PyxisProtocolHandler.h for the general disclaimer.
// This file additionally depends on nsIFile / inotify-watch primitives whose
// exact in-tree wrapper (nsIFileWatch? mozilla::FileWatcher? platform-specific
// like mozilla/dom/ipc/? ) should be confirmed against the pinned tree's
// netwerk/ or xpcom/io/ before implementing FinalizeReload/StartWatching for
// real. This header only commits to the public contract, not the watch impl.

#ifndef mozilla_net_PyxisOriginMap_h
#define mozilla_net_PyxisOriginMap_h

#include "nsISupportsImpl.h"
#include "nsString.h"
#include "nsTHashMap.h"
#include "nsHashKeys.h"
#include "mozilla/RefPtr.h"
#include "mozilla/Mutex.h"

namespace mozilla {
namespace net {

// Read-only, process-wide cache of Nova's app-id -> webroot mapping.
//
// Source of truth (nova-runtime-host-plan.md §4): Nova (a separate,
// privileged Rust daemon — NOT this process) writes
//     /run/pyxis/nova/origins.map
// as a plain-text table:
//     <app-id> TAB <absolute-webroot-path> NEWLINE
// on every registry change (install/uninstall/upgrade). This class treats
// that file as trusted config (per pyxis-scheme-and-kiosk-plan.md §A5:
// "Origins map trust: the map is written only by Nova into
// /run/pyxis/nova/ with restrictive perms; the handler treats it as trusted
// config and reloads on inotify change") — it does NOT re-validate app-id
// syntax or webroot existence beyond what's needed for safe path
// confinement in PyxisChannel; Nova already validated those at install time
// (nova-runtime-host-plan.md §3/§7).
//
// This class is deliberately dumb: no policy, no install/uninstall logic,
// just "read the map, serve lookups, refresh on change." Keeping policy out
// of the C++ side is what lets system vs. user install location stay a pure
// Nova concern (per the plan's explicit instruction) and keeps this file
// small and rebase-stable.
class PyxisOriginMap final {
  NS_INLINE_DECL_THREADSAFE_REFCOUNTING(PyxisOriginMap)

 public:
  static already_AddRefed<PyxisOriginMap> GetOrCreate();

  // Resolve an app-id (the host component of a pyxis:// URI) to its absolute
  // webroot directory. Returns false if the app-id is unknown (not a parse
  // error — the caller, PyxisChannel, must turn "unknown app" into a proper
  // NS_ERROR_FILE_NOT_FOUND-style channel failure, not a crash).
  //
  // aWebroot is returned WITHOUT any caller-supplied sub-path appended —
  // path-traversal confinement against the caller's requested sub-path is
  // PyxisChannel's job (§A4/§A5 of the scheme plan), not this class's; this
  // class only ever hands back paths that Nova itself wrote, so it does not
  // need to defend against traversal in the map file itself (Nova already
  // writes only realpath'd, validated entries at install time).
  bool Resolve(const nsACString& aAppId, nsCString& aWebroot);

  // Force a synchronous re-read of the origins map (used both by the
  // inotify-driven refresh path and by tests that want deterministic
  // reload timing without waiting on a filesystem watch).
  nsresult ReloadNow();

 private:
  PyxisOriginMap();
  ~PyxisOriginMap();

  nsresult StartWatching();
  void OnOriginsMapChanged();  // inotify callback -> ReloadNow()

  static constexpr auto kOriginsMapPath = "/run/pyxis/nova/origins.map";

  Mutex mLock{"PyxisOriginMap.mLock"};
  nsTHashMap<nsCStringHashKey, nsCString> mAppToWebroot;  // guarded by mLock
  bool mWatching = false;
};

}  // namespace net
}  // namespace mozilla

#endif  // mozilla_net_PyxisOriginMap_h
