#!/bin/bash
# Regenerate the Pyxis Gecko patch series from the live gecko-dev checkout.
#
# The checkout at target/gecko-src/gecko-dev is gitignored and multiple GB, so
# it is NOT the durable copy of the fork -- gecko/patches/pyxis/ is. This
# script keeps the two in sync: it rewrites every .PYXIS_DIFF as a real
# unified diff against the pinned upstream commit, and re-copies every
# Pyxis-only source file verbatim.
#
# Each .PYXIS_DIFF keeps its prose rationale as a header above the diff (git
# apply ignores leading non-diff text). Editing that prose by hand is fine and
# is preserved across regeneration; editing the diff below the separator is
# not -- change the checkout and re-run this instead.
#
# Usage:  scripts/regen-gecko-patches.sh [--check]
#   (no args)  rewrite the patch series from the checkout
#   --check    verify the series is in sync and applies cleanly; exit 1 if not.
#              Run this before committing a Gecko change.
set -u

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GECKO="${GECKO_SRC:-$REPO/target/gecko-src/gecko-dev}"
P="$REPO/gecko/patches/pyxis"
CHECK_ONLY=0
[ "${1:-}" = "--check" ] && CHECK_ONLY=1

if [ ! -d "$GECKO/.git" ]; then
  echo "error: no gecko checkout at $GECKO (set GECKO_SRC=)" >&2
  exit 2
fi
# Must be absolute: the script cd's into the checkout, after which a relative
# GECKO_SRC (e.g. the Makefile's default `target/gecko-src/gecko-dev`) would
# re-resolve against the checkout itself.
GECKO="$(cd "$GECKO" && pwd)"

SEP="════════════════════════════════════════════════════════════════════════
The unified diff below is generated from the checkout and is applied with
  git apply gecko/patches/pyxis/<path>.PYXIS_DIFF
(or \`patch -p1 <\`). Everything above this line is rationale, not content;
git apply ignores it. Regenerate with scripts/regen-gecko-patches.sh.
════════════════════════════════════════════════════════════════════════"

cd "$GECKO" || exit 2
mapfile -t MODIFIED < <(git status --porcelain --untracked-files=no | awk '{print $2}')

rc=0
tmpdir=$(mktemp -d); trap 'rm -rf "$tmpdir"' EXIT

for rel in "${MODIFIED[@]}"; do
  out="$P/$rel.PYXIS_DIFF"
  if [ ! -f "$out" ]; then
    echo "NEW      $rel (no existing note -- writing diff with a TODO header)"
    [ "$CHECK_ONLY" = 1 ] && { rc=1; continue; }
    mkdir -p "$(dirname "$out")"
    printf 'TODO: document why this change exists.\n' > "$tmpdir/prose"
  else
    awk '/^diff --git /{exit} /^════/{exit} {print}' "$out" > "$tmpdir/prose"
  fi

  { sed -e :a -e '/^\n*$/{$d;N;ba' -e '}' "$tmpdir/prose"; echo; echo "$SEP"; echo;
    git diff --binary -- "$rel"; } > "$tmpdir/new"

  if [ "$CHECK_ONLY" = 1 ]; then
    if ! diff -q "$tmpdir/new" "$out" >/dev/null 2>&1; then
      echo "STALE    $rel.PYXIS_DIFF does not match the checkout"; rc=1
    fi
  else
    cp "$tmpdir/new" "$out"; echo "wrote    $rel.PYXIS_DIFF"
  fi
done

# Pyxis-only source files: carried verbatim, not as diffs (no upstream side).
for rel in $(cd "$GECKO" && git status --porcelain | awk '/^\?\?/{print $2}' \
              | grep -E '^(netwerk/protocol/pyxis/|toolkit/actors/Pyxis)' ); do
  if [ -d "$GECKO/$rel" ]; then
    files=$(cd "$GECKO/$rel" && find . -type f | sed "s|^\./|$rel|")
  else
    files="$rel"
  fi
  for f in $files; do
    if [ "$CHECK_ONLY" = 1 ]; then
      diff -q "$GECKO/$f" "$P/$f" >/dev/null 2>&1 || { echo "STALE    $f (verbatim copy)"; rc=1; }
    else
      mkdir -p "$(dirname "$P/$f")"; cp "$GECKO/$f" "$P/$f"; echo "copied   $f"
    fi
  done
done

# Every diff must round-trip against pristine upstream, not just look right.
recon="$tmpdir/recon"; mkdir -p "$recon"
for pf in $(find "$P" -name "*.PYXIS_DIFF" | sort); do
  rel=${pf#"$P"/}; rel=${rel%.PYXIS_DIFF}
  mkdir -p "$recon/$(dirname "$rel")"
  git show "HEAD:$rel" > "$recon/$rel" 2>/dev/null || continue
  if ! (cd "$recon" && git apply "$pf" 2>/dev/null); then
    echo "BROKEN   $rel.PYXIS_DIFF does not apply to pinned upstream"; rc=1; continue
  fi
  diff -q "$recon/$rel" "$GECKO/$rel" >/dev/null 2>&1 \
    || { echo "BROKEN   $rel.PYXIS_DIFF applies but does not reproduce the checkout"; rc=1; }
done

[ "$rc" = 0 ] && echo "OK: patch series is in sync and reproduces the checkout from pinned upstream."
exit $rc
