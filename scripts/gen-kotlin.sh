#!/usr/bin/env bash
# Builds libsonorus_core for the host and generates the Kotlin bindings from it.
# Usage: scripts/gen-kotlin.sh [out-dir]   (default: target/kotlin). For reading the generated API only:
# the Android build generates its own copy (uniffiBindgen<Variant>), so never write into app/src.
set -euo pipefail
cd "$(dirname "$0")/.."
out="${1:-target/kotlin}"
cargo build -p sonorus-core
cargo run -q -p uniffi-bindgen -- generate \
    --library target/debug/libsonorus_core.so --language kotlin --no-format --out-dir "$out"
echo "Kotlin bindings written to $out"
