#!/bin/bash
# Package the iOS app sources for `iphone-use setup-app` as iphone-use-app.tar.gz:
# the app sources, both generated Xcode projects (users need no xcodegen) and the
# prebuilt libraries a user's Mac cannot build without a Rust toolchain.
#
#   scripts/package-app.sh <libipu_phone_host.a> <libipu_browser_session.a> <ipu-phone-host> [out.tar.gz]
#
# The Apple developer image is never packaged: setup-app copies it from the
# user's own Xcode. The on-phone daemon library ships with the runner sources.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOST_LIB="$1" BROWSER_LIB="$2" PAIR_TOOL="$3"
OUT="${4:-$ROOT/iphone-use-app.tar.gz}"
for f in "$HOST_LIB" "$BROWSER_LIB" "$PAIR_TOOL"; do
    [ -f "$f" ] || { echo "missing: $f" >&2; exit 1; }
done
command -v xcodegen >/dev/null || { echo "xcodegen is required" >&2; exit 1; }

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/app"
mkdir -p "$APP/apps/ios" "$APP/experiments/phone-self-host/Tunnel" "$APP/native" "$APP/bin"

(cd "$ROOT/apps/ios" && xcodegen generate --quiet --spec project.remote.yml && xcodegen generate --quiet --spec project.yml)
for item in IPhoneUseRemote AgentActivityWidget Shared LocalControlShared LocalControlTunnel \
            IPhoneUseRemoteTests IPhoneUseRemoteUITests IPhoneUseRemote.xcodeproj PhoneUseRemoteOnly.xcodeproj \
            project.yml project.remote.yml; do
    /usr/bin/ditto "$ROOT/apps/ios/$item" "$APP/apps/ios/$item"
done
mkdir -p "$APP/apps/ios/Tools" "$APP/apps/ios/SessionLibraries/iphoneos"
cp "$ROOT/apps/ios/Tools/build_single_app.py" "$ROOT/apps/ios/Tools/package_single_app.py" "$APP/apps/ios/Tools/"
cp "$BROWSER_LIB" "$APP/apps/ios/SessionLibraries/iphoneos/libipu_browser_session.a"
cp "$ROOT/experiments/phone-self-host/Tunnel/PhoneTunnelProvider.swift" "$APP/experiments/phone-self-host/Tunnel/"
cp "$ROOT/experiments/phone-self-host/prepare-image.py" "$ROOT/experiments/phone-self-host/THIRD-PARTY-NOTICES.txt" \
   "$APP/experiments/phone-self-host/"
cp "$HOST_LIB" "$APP/native/libipu_phone_host.a"
cp "$PAIR_TOOL" "$APP/bin/ipu-phone-host" && chmod 755 "$APP/bin/ipu-phone-host"

# Per-user Xcode state and anything Apple's never belong in the archive.
find "$APP" \( -name xcuserdata -o -name DeveloperImage -o -name .DS_Store \) -prune -exec rm -rf {} +
if find "$APP" -type l | grep -q .; then echo "links in the app tree" >&2; exit 1; fi
(cd "$STAGE" && COPYFILE_DISABLE=1 /usr/bin/tar -czf "$OUT" app)
echo "$OUT"
