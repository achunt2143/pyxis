/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// SCAFFOLD NOTICE: see PyxisProtocolHandler.h. The file-parsing logic below
// is real/complete by inspection; the inotify-watch wiring
// (StartWatching()/OnOriginsMapChanged()) is stubbed to a TODO because the
// exact in-tree async-file-watch primitive to use needs to be picked by
// looking at the actual pinned tree (candidates: mozilla::widget::
// FileSystemWatcher on some platforms, or a bare epoll/inotify fd pumped via
// the IO thread's event loop). Until that's wired, ReloadNow() is safe to
// call manually (e.g. on a timer, or on-demand before each lookup during
// early bring-up) — Resolve() never blocks on the watch existing.

#include "PyxisOriginMap.h"

#include "mozilla/ClearOnShutdown.h"
#include "mozilla/StaticPtr.h"
// MutexAutoLock is declared directly in mozilla/Mutex.h (included
// transitively via PyxisOriginMap.h) -- there is no separate
// mozilla/MutexAutoLock.h in this tree.
#include "nsIFile.h"
#include "nsIFileStreams.h"
#include "nsILineInputStream.h"
#include "nsNetUtil.h"
#include "nsPrintfCString.h"

namespace mozilla {
namespace net {

namespace {
// Named uniquely (not just "sSingleton") because Gecko's unified build
// concatenates multiple .cpp files into one translation unit -- an
// anonymous namespace no longer guarantees per-file isolation once that
// happens, and a same-named anonymous-namespace global in another file in
// the same unified unit (PyxisProtocolHandler.cpp) would otherwise redefine
// this one.
StaticRefPtr<PyxisOriginMap> sOriginMapSingleton;
}  // namespace

PyxisOriginMap::PyxisOriginMap() = default;
PyxisOriginMap::~PyxisOriginMap() = default;

/* static */
already_AddRefed<PyxisOriginMap> PyxisOriginMap::GetOrCreate() {
  if (!sOriginMapSingleton) {
    RefPtr<PyxisOriginMap> map = new PyxisOriginMap();
    // Best-effort initial load + watch start; a failure here (e.g. Nova
    // hasn't started yet and the file doesn't exist) must NOT be fatal —
    // it just means Resolve() returns false for everything until Nova
    // writes the file and the watch (or a later ReloadNow()) picks it up.
    // This matters at boot: Cronus's ordering doc says Nova comes up
    // "slightly later" than Echo/appHost and is "not on the critical path
    // for the compositor or shell chrome" (nova-runtime-host-plan.md §8) —
    // Gecko itself may well start before Nova has written anything.
    map->ReloadNow();
    map->StartWatching();
    sOriginMapSingleton = map;
    ClearOnShutdown(&sOriginMapSingleton);
  }
  RefPtr<PyxisOriginMap> result = sOriginMapSingleton;
  return result.forget();
}

bool PyxisOriginMap::Resolve(const nsACString& aAppId, nsCString& aWebroot) {
  MutexAutoLock lock(mLock);
  auto entry = mAppToWebroot.Lookup(aAppId);
  if (!entry) {
    return false;
  }
  aWebroot = *entry;
  return true;
}

nsresult PyxisOriginMap::ReloadNow() {
  nsCOMPtr<nsIFile> file;
  // NS_NewLocalFile wants UTF-16; kOriginsMapPath is a plain C string.
  // NS_NewUTF8LocalFile takes the UTF-8/narrow nsACString form directly.
  nsresult rv = NS_NewUTF8LocalFile(nsDependentCString(kOriginsMapPath),
                                    getter_AddRefs(file));
  NS_ENSURE_SUCCESS(rv, rv);

  bool exists = false;
  file->Exists(&exists);
  if (!exists) {
    // Not an error: Nova may not have started yet, or nothing is installed.
    // Leave whatever we last successfully loaded in place rather than
    // clearing it out from under in-flight loads on a transient read
    // failure — only a *successful* parse below replaces mAppToWebroot.
    return NS_OK;
  }

  nsCOMPtr<nsIInputStream> stream;
  rv = NS_NewLocalFileInputStream(getter_AddRefs(stream), file);
  NS_ENSURE_SUCCESS(rv, rv);

  nsCOMPtr<nsILineInputStream> lineStream = do_QueryInterface(stream);
  NS_ENSURE_STATE(lineStream);

  nsTHashMap<nsCStringHashKey, nsCString> parsed;
  nsAutoCString line;
  bool more = true;
  while (NS_SUCCEEDED(lineStream->ReadLine(line, &more))) {
    // Format: "<app-id>\t<absolute-webroot>". Blank lines and lines that
    // don't contain a tab are skipped rather than treated as a hard parse
    // failure — one malformed line (e.g. a concurrent partial write racing
    // our read) must not take down the whole map. Nova is expected to write
    // this file atomically (temp file + rename) precisely to make partial
    // reads rare, but defense in depth costs nothing here.
    if (!line.IsEmpty()) {
      int32_t tab = line.FindChar('\t');
      if (tab > 0 && static_cast<uint32_t>(tab) < line.Length() - 1) {
        nsAutoCString appId(Substring(line, 0, tab));
        nsAutoCString webroot(Substring(line, tab + 1));
        parsed.InsertOrUpdate(appId, webroot);
      }
    }
    if (!more) {
      break;
    }
  }

  {
    MutexAutoLock lock(mLock);
    mAppToWebroot = std::move(parsed);
  }
  return NS_OK;
}

nsresult PyxisOriginMap::StartWatching() {
  if (mWatching) {
    return NS_OK;
  }
  // TODO(pyxis): wire a real filesystem watch on kOriginsMapPath's parent
  // directory (/run/pyxis/nova/) that calls OnOriginsMapChanged() ->
  // ReloadNow() on any create/modify/rename event, matching
  // pyxis-scheme-and-kiosk-plan.md §A5's "reloads on inotify change"
  // requirement. Needs the pinned tree's actual watch primitive — grep
  // xpcom/io and widget/ at the pin for the in-tree pattern other
  // components use for "watch one file, reload on change" (mozLnk /
  // component registry hot-reload is a plausible existing precedent to
  // copy from). Until this lands, callers needing fresh data after an
  // install should call ReloadNow() explicitly (e.g. the bridge parent
  // actor can call it on a `nova.AppInstalled` Echo signal, giving
  // effectively the same freshness without needing inotify at all — that
  // may end up being the simpler final design; evaluate before spending
  // time on a raw inotify integration).
  mWatching = false;
  return NS_ERROR_NOT_IMPLEMENTED;
}

void PyxisOriginMap::OnOriginsMapChanged() { ReloadNow(); }

}  // namespace net
}  // namespace mozilla
