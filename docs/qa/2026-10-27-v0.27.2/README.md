# v0.27.2 artifact acceptance

Independently downloaded release assets passed all five payload SHA-256 checks, GitHub asset digests, bundle signing, notarization, stapled ticket, universal executable architectures and 40-tool MCP discovery. Standalone MCP bytes and six Native runner source files match the release bundle and tag. Exact hashes and Registry/marketplace snapshots are in [result.json](result.json).

Release CI: https://github.com/leeguooooo/iphone-use/actions/runs/40469818401

The bounded OCR change limits background recognition to one worker per daemon and reserves capacity before capturing a screenshot. Its cancellation, retry, failure and panic behavior passed automated checks in PR #362. This artifact audit does not establish OCR speed or physical-device acceptance for v0.27.2. The preceding v0.27.1 physical reader run remains separately recorded.

The i13 named instance was subsequently upgraded with `--no-setup`. All three installed executables match the downloaded release byte-for-byte and bundle signing verifies. Device targets and ports were preserved; the default instance stayed on v0.27.1. A live status read reports v0.27.2, released and not drivable. No reconnect was requested.
