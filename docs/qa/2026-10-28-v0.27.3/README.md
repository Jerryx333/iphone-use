# v0.27.3 release and installed-artifact acceptance

Five downloaded payloads match their SHA-256 files and GitHub asset digests. The Mac App passes deep/strict signature verification, Developer ID validation, Gatekeeper and stapled-ticket validation. Its three executables and the standalone MCP are universal; the standalone MCP bytes match the embedded copy. Six Native source files match the exact release tag. The auxiliary phone-host executable and static libraries were checked separately. See [result.json](result.json).

The actual downloaded MCP initializes as 0.27.3 with 40 tools. Against an owned loopback HTTP fixture, `phone_tap_label` for `Conversas` returns the complete `Conversas, tab, 2 of 3` suggestion and sends no mutation request.

The default and i13 Mac runtimes were upgraded through the release-pinned installer with `--no-setup`. Their three executables match the audited download byte-for-byte and bundle signatures verify. Captured device-target, daemon/control-port and authentication settings, plus park-marker state, are preserved. Live status reports 0.27.3 for both, released and unowned. The agent skill is pinned to the same release commit.

The original release build/sign/notarize/upload job passed. Its optional Registry job never acquired a macOS runner and was cancelled. Linux recovery run 40538645937 skipped the binary build, validated the existing release and published the Registry entry. The exact Registry endpoint is active at 0.27.3; marketplace sync 40539061235 succeeded at the same version.

This round sent no phone input and requested no reconnect. i13's pre-existing Native build failure remains; this is Mac installation acceptance, not physical-device acceptance. The experimental per-character input branch is outside this release.
