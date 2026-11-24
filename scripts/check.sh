#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/opendex-target}"
find "$CARGO_TARGET_DIR" -type f -name '._*' -delete 2>/dev/null || true

node --test 'tests'/*.mjs
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo test --locked --no-default-features --all-targets
