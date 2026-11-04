# 2026-10-26 release closeout

Four incremental releases were published and independently downloaded:

| Release | Scope | MCP tools |
| --- | --- | --- |
| v0.24.0 | Park after phone-side exit; pause/resume; notification lifecycle | 38 |
| v0.24.1 | Named-instance target recovery and consistent installer guidance | 38 |
| v0.25.1 | Per-call MCP instance selection and explicit notification setup | 39 |
| v0.25.2 | Direct loopback MCP transport, credential target checks, stopped status | 39 |

For each, all five archive checksums match the published SHA-256 files and
GitHub digests. App signatures, notarization assessment, stapled tickets,
architectures, exact versions, embedded/standalone MCP initialization and tool
lists were independently checked. Packaged Native source anchors match each tag.
[Results and hashes](releases.json).

The installed App/CLI match the downloaded v0.25.2. Both default and i13 daemon
APIs report 0.25.2. A fresh installed MCP exposes 39 tools and reads both real
instances even with the process proxy pointing at an unavailable port. These
are read-only metadata calls; no UI input or reconnect is requested. The iPhone
17 target configuration and i13 phone-ended marker remain unchanged. Both
instances remain released; i13 is phone-ended and not drivable.

[Physical volume-key acceptance](../2026-10-26-phone-exit/README.md) separately
records v0.24.1 on iPhone 13/iOS 27.0 over USB, with 46.13 seconds of parking.
The private character-input work remains Draft #335. PR #346's stopped display
fix is included; ambiguous wireless-failure automatic recovery remains under
changes-requested review. No complete competitive-superiority claim is made.

The v0.25.0 attempt stopped at an obsolete release-only tool-count assertion
before creating a Release. PR, Rust and release checks now use one tool-name
fixture, and release verification exercises the bundled instance router and panel.
