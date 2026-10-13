#!/bin/bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
: "${UDID:?Set UDID to the USB phone identifier}"
: "${DEVELOPMENT_TEAM:?Set DEVELOPMENT_TEAM to your Apple development team (free Apple ID supported)}"
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export CARGO_BUILD_JOBS=2
"$HERE/build-lib.sh"
cd "$HERE"
xcodegen generate
xcodebuild -project SelfRunnerSpike.xcodeproj -scheme SelfRunnerSpike -sdk iphoneos \
  -configuration Debug -destination "id=$UDID" -allowProvisioningUpdates \
  -derivedDataPath "$HERE/build/DerivedData" DEVELOPMENT_TEAM="$DEVELOPMENT_TEAM" build
APP="$HERE/build/DerivedData/Build/Products/Debug-iphoneos/SelfRunnerSpike.app"
xcrun devicectl device install app --device "$UDID" "$APP"
PAIRING="$HERE/build/pairing/rp_pairing.plist"
if [[ ! -f "$PAIRING" ]]; then
  mkdir -p "$HERE/build/pairing"
  cd "$ROOT"
  if [[ -z "${CARGO_HOME:-}" && -d /private/tmp/self-boot-cargo-home/registry ]]; then
    export CARGO_HOME=/private/tmp/self-boot-cargo-home
  fi
  cargo build -p self-boot --bin self-boot-probe --offline
  "$ROOT/target/debug/self-boot-probe" rp-pair --udid "$UDID" \
    --name "${RP_HOST_NAME:-$(hostname)-selfrunner}" --out "$PAIRING"
fi
xcrun devicectl device copy to --device "$UDID" --domain-type appDataContainer \
  --domain-identifier dev.iphoneuse.selfrunner.spike --source "$PAIRING" \
  --destination Documents/rp_pairing.plist
