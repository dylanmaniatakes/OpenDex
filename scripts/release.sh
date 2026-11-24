#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/opendex-target}"
find "$CARGO_TARGET_DIR" -type f -name '._*' -delete 2>/dev/null || true
find dist -type f -name '._*' -delete 2>/dev/null || true

bash scripts/check.sh
bash scripts/bundle-macos.sh release
find dist -type f -name '._*' -delete 2>/dev/null || true
python3 scripts/verify-release.py
