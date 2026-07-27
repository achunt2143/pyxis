#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 /path/to/project" >&2
  exit 2
fi

PROJECT_DIR="$(cd "$1" && pwd)"
PYXIS_JSON="$PROJECT_DIR/pyxis.json"
OUT_DIR="$PROJECT_DIR/.pyxis"
STAGE_DIR="$OUT_DIR/stage"
PKG_DIR="$OUT_DIR/pkg"
ARTIFACT_DIR="$PROJECT_DIR/dist"

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || { echo "missing command: $1" >&2; exit 3; }
}

require_cmd jq
require_cmd python3
require_cmd tar

if [[ ! -f "$PYXIS_JSON" ]]; then
  echo "missing pyxis.json in $PROJECT_DIR" >&2
  exit 4
fi

runtime="$(jq -r '.runtime' "$PYXIS_JSON")"
app_id="$(jq -r '.id' "$PYXIS_JSON")"
version="$(jq -r '.version' "$PYXIS_JSON")"
title="$(jq -r '.title' "$PYXIS_JSON")"
vendor="$(jq -r '.vendor' "$PYXIS_JSON")"
icon="$(jq -r '.icon' "$PYXIS_JSON")"
section="$(jq -r '.package.section' "$PYXIS_JSON")"
priority="$(jq -r '.package.priority' "$PYXIS_JSON")"
arch="$(jq -r '.package.architecture' "$PYXIS_JSON")"
description="$(jq -r '.package.description // .title' "$PYXIS_JSON")"
maintainer_email="$(jq -r '.package.maintainerEmail // "devnull@example.invalid"' "$PYXIS_JSON")"

auth_maintainer="$vendor <$maintainer_email>"

rm -rf "$OUT_DIR"
mkdir -p "$STAGE_DIR" "$PKG_DIR/CONTROL" "$ARTIFACT_DIR"

app_root="$PKG_DIR/usr/pyxis/applications/$app_id"
mkdir -p "$app_root"

copy_icon() {
  install -Dm644 "$PROJECT_DIR/$icon" "$app_root/$(basename "$icon")"
}

generate_appinfo_web() {
  local entry
  entry="$(jq -r '.entry' "$PYXIS_JSON")"
  jq -n \
    --arg id "$app_id" \
    --arg version "$version" \
    --arg vendor "$vendor" \
    --arg title "$title" \
    --arg main "$entry" \
    --arg icon "$(basename "$icon")" \
    '{id:$id,version:$version,vendor:$vendor,title:$title,type:"web",main:$main,icon:$icon,uiRevision:1}' \
    > "$app_root/appinfo.json"
}

generate_appinfo_native() {
  local install_mode bin_name main_path
  install_mode="$(jq -r '.pdk.installMode' "$PYXIS_JSON")"
  bin_name="$(jq -r '.pdk.binaryName' "$PYXIS_JSON")"
  if [[ "$install_mode" == "bundled-app" ]]; then
    main_path="/opt/pyxis/apps/$app_id/bin/$bin_name"
  else
    main_path="/usr/bin/$bin_name"
  fi
  jq -n \
    --arg id "$app_id" \
    --arg version "$version" \
    --arg vendor "$vendor" \
    --arg title "$title" \
    --arg main "$main_path" \
    --arg icon "$(basename "$icon")" \
    '{id:$id,version:$version,vendor:$vendor,title:$title,type:"native",main:$main,icon:$icon,uiRevision:1}' \
    > "$app_root/appinfo.json"
}

generate_control() {
  local depends_field=""
  if jq -e '.package.depends and (.package.depends|length>0)' "$PYXIS_JSON" >/dev/null; then
    depends_field="Depends: $(jq -r '.package.depends | join(", ")' "$PYXIS_JSON")"
  fi
  {
    echo "Package: $app_id"
    echo "Version: $version"
    echo "Architecture: $arch"
    echo "Section: $section"
    echo "Priority: $priority"
    echo "Maintainer: $auth_maintainer"
    [[ -n "$depends_field" ]] && echo "$depends_field"
    echo "Description: $description"
  } > "$PKG_DIR/CONTROL/control"
}

generate_postinst() {
  cat > "$PKG_DIR/CONTROL/postinst" <<'POSTINST'
#!/bin/sh
set -e
if command -v pyxis-appctl >/dev/null 2>&1; then
  pyxis-appctl rescan || true
fi
if command -v pyxis-cachectl >/dev/null 2>&1; then
  pyxis-cachectl refresh-icons || true
fi
exit 0
POSTINST
  chmod +x "$PKG_DIR/CONTROL/postinst"
}

build_ipk() {
  local pkg_name tmpfile
  pkg_name="${app_id}_${version}_${arch}.ipk"
  tmpfile="$ARTIFACT_DIR/$pkg_name"
  python3 - <<PY
import os, tarfile, io
pkg_dir = os.path.abspath("$PKG_DIR")
out_file = os.path.abspath("$tmpfile")
control_dir = os.path.join(pkg_dir, "CONTROL")
with tarfile.open(os.path.join("$OUT_DIR", "control.tar.gz"), "w:gz") as tf:
    tf.add(control_dir, arcname=".")
with tarfile.open(os.path.join("$OUT_DIR", "data.tar.gz"), "w:gz") as tf:
    for root, dirs, files in os.walk(pkg_dir):
        rel = os.path.relpath(root, pkg_dir)
        if rel == "CONTROL" or rel.startswith("CONTROL/"):
            continue
        for d in dirs:
            p = os.path.join(root, d)
            arc = os.path.relpath(p, pkg_dir)
            tf.add(p, arcname=arc)
        for f in files:
            p = os.path.join(root, f)
            arc = os.path.relpath(p, pkg_dir)
            tf.add(p, arcname=arc)
with open(os.path.join("$OUT_DIR", "debian-binary"), "wb") as fh:
    fh.write(b"2.0\n")
with tarfile.open(out_file, "w:gz") as tf:
    tf.add(os.path.join("$OUT_DIR", "debian-binary"), arcname="debian-binary")
    tf.add(os.path.join("$OUT_DIR", "control.tar.gz"), arcname="control.tar.gz")
    tf.add(os.path.join("$OUT_DIR", "data.tar.gz"), arcname="data.tar.gz")
PY
  echo "$tmpfile"
}

package_web() {
  local out_dir entry
  out_dir="$(jq -r '.sdk.outDir // "dist"' "$PYXIS_JSON")"
  entry="$(jq -r '.entry' "$PYXIS_JSON")"
  if [[ ! -d "$PROJECT_DIR/$out_dir" ]]; then
    echo "expected built web output at $PROJECT_DIR/$out_dir" >&2
    exit 5
  fi
  cp -a "$PROJECT_DIR/$out_dir/." "$app_root/"
  copy_icon
  generate_appinfo_web
}

package_native() {
  local bin_name install_mode build_dir
  build_dir="$(jq -r '.buildDir // "build"' "$PYXIS_JSON")"
  bin_name="$(jq -r '.pdk.binaryName' "$PYXIS_JSON")"
  install_mode="$(jq -r '.pdk.installMode' "$PYXIS_JSON")"
  if [[ ! -f "$PROJECT_DIR/$build_dir/$bin_name" ]]; then
    echo "expected native binary at $PROJECT_DIR/$build_dir/$bin_name" >&2
    exit 6
  fi
  if [[ "$install_mode" == "bundled-app" ]]; then
    install -Dm755 "$PROJECT_DIR/$build_dir/$bin_name" "$PKG_DIR/opt/pyxis/apps/$app_id/bin/$bin_name"
  else
    install -Dm755 "$PROJECT_DIR/$build_dir/$bin_name" "$PKG_DIR/usr/bin/$bin_name"
  fi
  copy_icon
  generate_appinfo_native
}

case "$runtime" in
  web) package_web ;;
  native) package_native ;;
  *) echo "unsupported runtime: $runtime" >&2; exit 7 ;;
esac

generate_control
generate_postinst
artifact="$(build_ipk)"
echo "created: $artifact"
