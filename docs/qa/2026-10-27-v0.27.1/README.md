# v0.27.1 artifact and physical reader acceptance

The independently downloaded release passes payload hashes, signing/notarization, universal architectures, Native-source equality and 40-tool MCP discovery. The i13 installed binaries were compared byte-for-byte with this release before the device run. Exact Registry and current marketplace evidence are in [result.json](result.json).

On a physical iPhone 13 running iOS 27.0, all 10 owned-fixture reader checks passed. The snapshot exposed 512 UTF-16 units with value_truncated; phone_read_value returned the full 6028-unit App seed with the identical SHA-256 in full/slim/off output modes, with one value copy and no competing structured payload. Emoji boundaries, quoted/newline/tab labels, secure and ambiguous/missing selections and boolean values were exercised. App-owned before/after telemetry was identical: zero editing events and no focus change.

The first legacy-lifecycle fixture did not become foreground; an independent console launch exited with code 0. The fixture was migrated to UIScene with build version 2 before any read was credited. Apple documents that latest-SDK apps need this lifecycle on iOS 27: [UIKit scene migration](https://developer.apple.com/documentation/uikit/transitioning-to-the-uikit-scene-based-life-cycle). The executed fixture source is scripts/value-read-fixture.swift; source and binary hashes are retained in the JSON.

The owned fixture was uninstalled, absence was read back, and the test lease release was verified. Screenshots and raw device logs stay private. This covers the selected-value reader, not text input, per-character isolation, other devices/OS versions, or a comparative task-speed benchmark.
