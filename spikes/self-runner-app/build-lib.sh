#!/bin/bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export CARGO_BUILD_JOBS=2
export IPHONEOS_DEPLOYMENT_TARGET=17.0
# Override CARGO_HOME if the sandbox needs a writable cache; no shared target directory.
if [[ -z "${CARGO_HOME:-}" && -d /private/tmp/self-boot-cargo-home/registry ]]; then
  export CARGO_HOME=/private/tmp/self-boot-cargo-home
fi
mkdir -p "$HERE/build"
cd "$ROOT"
# Follow repository machine-level build throttling when installed.
CARGO_RUNNER=(cargo)
if [[ -x "$HOME/.local/bin/heavy-cargo" ]]; then
  CARGO_RUNNER=("$HOME/.local/bin/heavy-cargo")
fi
"${CARGO_RUNNER[@]}" build -p self-boot --release --target aarch64-apple-ios --lib --offline
cp "$ROOT/target/aarch64-apple-ios/release/libself_boot.a" "$HERE/build/libself_boot.a"
