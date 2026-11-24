#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
  echo "The macOS bundle must be built on macOS." >&2
  exit 1
fi

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/opendex-target}"
export COPYFILE_DISABLE=1

profile="${1:-release}"
case "$profile" in
  release) cargo build --locked --release ;;
  debug) cargo build --locked ;;
  *) echo "Usage: scripts/bundle-macos.sh [release|debug]" >&2; exit 1 ;;
esac

work="$(mktemp -d "${TMPDIR:-/tmp}/opendex-bundle.XXXXXX")"
trap 'rm -rf "$work"' EXIT
bundle="$work/OpenDex.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources" "$work/OpenDex.iconset"
cp -X "$CARGO_TARGET_DIR/$profile/opendex" "$bundle/Contents/MacOS/opendex"

swift scripts/icon.swift 1024 "$work/master.png"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$work/master.png" --out "$work/OpenDex.iconset/icon_${size}x${size}.png" >/dev/null
  sips -z "$((size * 2))" "$((size * 2))" "$work/master.png" --out "$work/OpenDex.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$work/OpenDex.iconset" -o "$bundle/Contents/Resources/OpenDex.icns"

python3 - "$bundle/Contents/Info.plist" <<'PY'
import json
import plistlib
import sys

with open("tauri.conf.json") as config_file:
    config = json.load(config_file)
with open(sys.argv[1], "wb") as plist:
    plistlib.dump({
        "CFBundleName": config["productName"],
        "CFBundleDisplayName": config["productName"],
        "CFBundleExecutable": "opendex",
        "CFBundleIdentifier": config["identifier"],
        "CFBundleShortVersionString": config["version"],
        "CFBundleVersion": config["version"],
        "CFBundlePackageType": "APPL",
        "CFBundleIconFile": "OpenDex.icns",
        "LSApplicationCategoryType": "public.app-category.developer-tools",
        "LSMinimumSystemVersion": "13.0",
        "NSHighResolutionCapable": True,
    }, plist)
PY
printf APPL???? > "$bundle/Contents/PkgInfo"
codesign --force --sign - "$bundle"
codesign --verify --strict "$bundle"

output="${OPENDEX_BUNDLE_DIR:-$PWD/dist}"
mkdir -p "$output"
destination="$output/OpenDex.app"
if [[ -e "$destination" ]] && [[ ! -f "$destination/Contents/MacOS/opendex" ]]; then
  echo "An unrelated bundle already exists at $destination." >&2
  exit 1
fi
/usr/bin/ditto --norsrc --noextattr --noqtn "$bundle" "$destination"
codesign --verify --strict "$destination"

build_manifest="$output/OpenDex-build.json"
version="$(python3 -c 'import json; print(json.load(open("tauri.conf.json"))["version"])')"
python3 scripts/build-manifest.py "$build_manifest" "$destination" "$version" "$profile"
printf '%s\n' "$destination"
