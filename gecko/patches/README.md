# Pyxis Gecko patch series

This directory is the **durable copy of the vela fork**. The build checkout at
`target/gecko-src/gecko-dev/` is gitignored and multiple GB — it is a build
artifact, not the source of truth. Everything Pyxis changes about Gecko must be
reconstructible from this directory alone.

Verified: applying this series to the pinned upstream commit reproduces the
current checkout **byte-identically** across all 12 modified files.

## Contents

Two kinds of file, applied differently:

| Kind | What | How it's applied |
|---|---|---|
| `<path>.PYXIS_DIFF` | A modified upstream file | `git apply` — real unified diff, with prose rationale as a header above a separator line (git apply ignores leading non-diff text) |
| Everything else under `pyxis/` | A Pyxis-only file with no upstream counterpart | Copied verbatim into the checkout |

## Reconstructing the fork from scratch

```sh
git clone https://github.com/mozilla/gecko-dev
cd gecko-dev
git checkout 5836a062726f715fda621338a17b51aff30d0a8c -b central-work   # see gecko/GECKO_PIN

# 1. Pyxis-only sources (no upstream side, so not diffs)
cp -r /path/to/geckos/gecko/patches/pyxis/netwerk/protocol/pyxis netwerk/protocol/
cp /path/to/geckos/gecko/patches/pyxis/toolkit/actors/Pyxis*.sys.mjs toolkit/actors/

# 2. Modified upstream files
find /path/to/geckos/gecko/patches/pyxis -name '*.PYXIS_DIFF' -print0 \
  | xargs -0 -n1 git apply

MOZCONFIG=/path/to/geckos/gecko/mozconfig-vela ./mach build
```

## Keeping it in sync

The checkout is where you edit; this directory is generated from it.

```sh
make gecko-patches-regen   # rewrite the series from the checkout
make gecko-patches-check   # verify in sync; fails the build if not
make install-hooks         # opt in to the pre-commit guard (one time)
```

The check is wired into `make check` and into `gecko-build`, so a normal
`make build-release` cannot silently ship a Gecko change that isn't captured
here. It skips cleanly (exit 2) when there's no checkout, so it costs nothing
for people who never touch the fork.

`make install-hooks` additionally points `core.hooksPath` at
`scripts/git-hooks/`, whose `pre-commit` runs the same check — worth doing if
you edit Gecko often, since the failure mode is losing work rather than
breaking a build. Bypass a single commit with `git commit --no-verify`.

The check does more than compare text: it re-applies every diff to a pristine
copy of the pinned upstream file and asserts the result reproduces the
checkout, so it catches a diff that still applies but no longer produces the
right result.

Prose headers are preserved across regeneration — edit them freely. Do not edit
the diff below the separator; change the checkout and regenerate instead.

## Why some notes read like essays

Several of these changes fix bugs that fail **silently** — no error, no crash,
the feature just never happens (`toolkit/actors/moz.build` not packaging the
bridge actors; `pyxis` missing from `HostLocatorSchemes()` so the actor's match
pattern silently matched nothing). The prose records how each was tracked down,
because the diffs themselves are two-line changes that look inconsequential and
are easy to drop on a rebase.
