# v0.26.1 downloaded-artifact acceptance

The published GitHub release was downloaded independently on 2026-10-27. All five payload hashes match their checksum files and GitHub asset digests. The macOS app passes strict signing verification, notarized Developer ID assessment and stapled-ticket validation. Embedded and standalone MCP initialize successfully and expose 39 tools. Shipped Native source hashes match the release tag.

The tag contains #356 (retain the resolved app in confirmed batch steps after a later failure) and #357 (allow transient reconnect blockers to settle). It does not contain the later full-value reader or #355's privacy/deadline changes.

[result.json](result.json) records artifact evidence. The MCP Registry and plugin marketplace both independently report 0.26.1; the JSON records the verification time and marketplace content hash. No installation or physical-device result is inferred from these artifact checks.
