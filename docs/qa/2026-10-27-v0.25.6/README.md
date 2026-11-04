# v0.25.6 release acceptance

PR #349 skips installed-app compatibility lookup when a screen has no registry
flows. Missing/empty/null flow lists and a UI label containing registry are
covered through real stdio MCP; a nonempty flow list retains inventory lookup.
The metrics integration test now counts four actual backend calls instead of
five, while its model-round-trip assertion remains unchanged. Complete PR checks
and security scanning pass.

An isolated daemon delayed its apps response by three seconds. Published v0.25.5
requested elements, apps and status in 3.016 seconds; the downloaded and installed
v0.25.6 requested only elements in 0.002 seconds. Both returned the read normally.
This is a bounded metadata-overhead probe, not an App startup/loading benchmark.

All five payload checksums, app signing/notarization/stapled ticket, universal
binaries, auxiliary tools, iOS library architectures and six Runner tag sources
pass independent verification. Embedded and standalone MCP report 0.25.6 with
39 tools. Registry and plugin marketplace both report 0.25.6.

Both local daemons and a fresh MCP report 0.25.6. Before upgrade both instances
were released, offline and unowned, and i13 had no parking marker after subsequent
requests. The upgrade preserves those current states and the default phone target;
it does not recreate the historical exit marker. No phone input, unlock,
reconnect or new physical-device acceptance was performed.
