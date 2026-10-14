#!/bin/bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/remote-transport-check.XXXXXX")"
trap 'rm -rf "$work"' EXIT
sdk="$(xcrun --sdk macosx --show-sdk-path)"
xcrun swiftc -sdk "$sdk" -module-cache-path "$work/module-cache" \
  "$root/Sources/RemotePairing.swift" "$root/Sources/RemoteTransport.swift" \
  "$root/Sources/RemoteLink.swift" "$root/Sources/RemoteTunnel.swift" \
  "$root/tests/main.swift" -o "$work/check"
"$work/check"
