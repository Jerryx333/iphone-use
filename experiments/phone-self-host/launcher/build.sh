#!/bin/sh
set -eu
cd "$(dirname "$0")"
python3 ../prepare-dependency.py
BUILDER="$HOME/.local/bin/heavy-cargo"
if [ ! -x "$BUILDER" ]; then BUILDER=cargo; fi
IPHONEOS_DEPLOYMENT_TARGET=17.4 "$BUILDER" build --release --lib --target aarch64-apple-ios
