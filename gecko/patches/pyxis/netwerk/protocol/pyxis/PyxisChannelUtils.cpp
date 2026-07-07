/* -*- Mode: C++; tab-width: 2; indent-tabs-mode: nil; c-basic-offset: 2 -*- */
/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// See PyxisChannelUtils.h for why this exists. Body is moved verbatim (modulo
// s/URI()-and-uri->Get.../aAppId+aPath params/) from the pre-refactor
// PyxisChannel::ResolveAndConfinePath / PyxisChannel::InferContentType --
// the path-traversal defense logic itself is UNCHANGED, only its calling
// convention (member function reading mOriginMap/URI() -> static function
// taking explicit params) moved, so both PyxisChannel::OpenContentStream and
// PyxisChannelParent::RecvGetContent can call the exact same code path.

#include "PyxisChannelUtils.h"
#include "PyxisOriginMap.h"

#include "nsIFile.h"
#include "nsNetUtil.h"

namespace mozilla {
namespace net {

/* static */
nsresult PyxisChannelUtils::ResolveAndConfinePath(
    PyxisOriginMap* aOriginMap, const nsACString& aAppId,
    const nsACString& aPath, nsCString& aResolvedAbsolutePath) {
  if (aAppId.IsEmpty()) {
    return NS_ERROR_MALFORMED_URI;
  }

  nsAutoCString webroot;
  if (!aOriginMap || !aOriginMap->Resolve(aAppId, webroot)) {
    // Unknown app-id: Nova hasn't registered it (or it was uninstalled
    // between link-time and load-time). Proper load error, not a crash.
    return NS_ERROR_FILE_NOT_FOUND;
  }

  nsAutoCString path(aPath);

  // nsIURI::filePath includes the leading '/' (e.g. "/index.html"), but
  // nsIFile::AppendRelativePath below requires a fragment with NO leading
  // slash -- it hard-rejects one as NS_ERROR_FILE_UNRECOGNIZED_PATH (found
  // via a real failed asyncOpen(), not by inspection; see
  // nsLocalFileUnix.cpp's AppendRelativeNativePath). Strip it here, once,
  // rather than at each call site.
  if (!path.IsEmpty() && path.First() == '/') {
    path.Cut(0, 1);
  }

  // --- Path-traversal defense (the highest-severity bug class here) ---
  // Strategy: build the naive concatenated path, then realpath() (via
  // nsIFile::Normalize(), which resolves ".."/"." components AND symlinks
  // on POSIX) both the webroot and the candidate, and assert the candidate
  // remains prefixed by the webroot after normalization. Reject up front on
  // any raw ".." path segment as a fast, defense-in-depth pre-check too
  // (belt + suspenders — the realpath check below is the one that actually
  // matters for symlink-based escapes, which a string-level ".." check
  // alone would miss).
  if (path.Find("..") != kNotFound) {
    // A raw ".." anywhere in the path is rejected outright rather than
    // trying to distinguish "legitimate" occurrences (e.g. a filename that
    // merely contains the substring "..") from traversal attempts — app
    // webroots are populated by Nova's installer from a controlled
    // manifest/tarball, so there is no legitimate reason for a real
    // in-webroot path to need literal ".." segments, and refusing the
    // substring outright is simpler and safer than correct path-segment
    // splitting logic that could have its own bugs.
    return NS_ERROR_MALFORMED_URI;
  }

  nsCOMPtr<nsIFile> webrootDir;
  // NS_NewLocalFile wants UTF-16 (nsAString); webroot is a UTF-8 nsCString
  // (as returned by PyxisOriginMap::Resolve) — NS_NewUTF8LocalFile is the
  // UTF-8-native counterpart, not a NS_ConvertUTF8toUTF16 + NS_NewLocalFile
  // detour.
  nsresult rv = NS_NewUTF8LocalFile(webroot, getter_AddRefs(webrootDir));
  NS_ENSURE_SUCCESS(rv, rv);
  rv = webrootDir->Normalize();
  NS_ENSURE_SUCCESS(rv, rv);

  nsCOMPtr<nsIFile> candidate;
  rv = webrootDir->Clone(getter_AddRefs(candidate));
  NS_ENSURE_SUCCESS(rv, rv);
  // AppendRelativePath (rather than manual string concatenation) rejects
  // absolute-path re-rooting and further ".."/"." segments itself on most
  // platforms' nsIFile implementations — but do not rely on that alone;
  // the explicit realpath-prefix check below is the actual authoritative
  // guard.
  rv = candidate->AppendRelativePath(NS_ConvertUTF8toUTF16(path));
  NS_ENSURE_SUCCESS(rv, rv);
  rv = candidate->Normalize();
  NS_ENSURE_SUCCESS(rv, rv);

  nsAutoCString webrootNormalized;
  webrootDir->GetNativePath(webrootNormalized);
  nsAutoCString candidateNormalized;
  candidate->GetNativePath(candidateNormalized);

  if (!StringBeginsWith(candidateNormalized, webrootNormalized)) {
    // Normalization resolved the candidate outside the webroot (via ".."
    // segments, a symlink escape, or similar) — deny, don't attempt to
    // "fix up" the path.
    return NS_ERROR_FILE_ACCESS_DENIED;
  }

  aResolvedAbsolutePath = candidateNormalized;
  return NS_OK;
}

/* static */
void PyxisChannelUtils::InferContentType(const nsACString& aPath,
                                         nsACString& aOutMime) {
  // Small, explicit allowlist (plan §A4 point 3) — NOT content-sniffed, and
  // NOT derived from a general-purpose OS mime database (which can include
  // executable/script types we don't want to ever serve with an inferred
  // type from this handler). Default is application/octet-stream for
  // anything unrecognized, which browsers/embedders will not execute as
  // script/HTML.
  struct Entry {
    const char* ext;
    const char* mime;
  };
  static constexpr Entry kTable[] = {
      {".html", "text/html"},
      {".htm", "text/html"},
      {".js", "text/javascript"},
      {".mjs", "text/javascript"},
      {".css", "text/css"},
      {".json", "application/json"},
      {".wasm", "application/wasm"},
      {".svg", "image/svg+xml"},
      {".png", "image/png"},
      {".jpg", "image/jpeg"},
      {".jpeg", "image/jpeg"},
      {".gif", "image/gif"},
      {".webp", "image/webp"},
      {".woff", "font/woff"},
      {".woff2", "font/woff2"},
      {".ttf", "font/ttf"},
  };

  for (const auto& entry : kTable) {
    if (StringEndsWith(aPath, nsDependentCString(entry.ext),
                        nsCaseInsensitiveCStringComparator)) {
      aOutMime.AssignASCII(entry.mime);
      return;
    }
  }
  aOutMime.AssignLiteral("application/octet-stream");
}

/* static */
nsresult PyxisChannelUtils::OpenConfinedFile(
    const nsACString& aResolvedAbsolutePath, nsIInputStream** aStream,
    int64_t* aContentLength) {
  nsCOMPtr<nsIFile> file;
  nsresult rv = NS_NewUTF8LocalFile(aResolvedAbsolutePath, getter_AddRefs(file));
  NS_ENSURE_SUCCESS(rv, rv);

  bool exists = false;
  file->Exists(&exists);
  bool isDir = false;
  file->IsDirectory(&isDir);
  if (!exists || isDir) {
    return NS_ERROR_FILE_NOT_FOUND;
  }

  int64_t size = -1;
  file->GetFileSize(&size);

  nsCOMPtr<nsIInputStream> fileStream;
  // NS_NewLocalFileInputStream is safe to hand to nsBaseChannel's pump
  // machinery for async reads (blocking streams get read on a background
  // I/O thread by nsInputStreamPump automatically) — matches nsFileChannel's
  // own MakeFileInputStream approach at this pin. This is ALSO the exact
  // same call the parent-brokered IPDL path (PyxisChannelParent) uses to
  // produce the stream it hands back to the content process — the
  // read-off-main-thread behavior nsInputStreamPump gives a blocking stream
  // is just as relevant there, since PyxisChannelParent::RecvGetContent
  // still runs on the parent's main thread.
  rv = NS_NewLocalFileInputStream(getter_AddRefs(fileStream), file);
  NS_ENSURE_SUCCESS(rv, rv);

  if (aContentLength) {
    *aContentLength = size;
  }
  fileStream.forget(aStream);
  return NS_OK;
}

}  // namespace net
}  // namespace mozilla
