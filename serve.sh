#!/usr/bin/env bash
# Convenience wrapper that runs the release binary.
# Builds it first if missing.
set -euo pipefail

cd "$(dirname "$0")"

BIN="target/release/kanaweb-server"

if [[ ! -x "$BIN" ]]; then
    echo "→ binary not found, building (cargo build --release)…"
    cargo build --release
fi

exec "./$BIN" "$@"
