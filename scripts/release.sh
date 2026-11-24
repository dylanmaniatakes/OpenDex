#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/opendex-target}"

bash scripts/check.sh
bash scripts/bundle-macos.sh release
python3 scripts/verify-release.py
