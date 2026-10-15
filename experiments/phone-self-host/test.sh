#!/bin/sh
set -eu
cd "$(dirname "$0")"
TEST_DIR=$(mktemp -d)
trap 'rm -rf "$TEST_DIR"' EXIT
xcrun swiftc Shared/RemotePairingHandshake.swift Shared/LocalPacketReflection.swift Shared/RunnerSigning.swift Shared/PairRecordImport.swift Tests/main.swift -o "$TEST_DIR/protocol-test"
"$TEST_DIR/protocol-test"
