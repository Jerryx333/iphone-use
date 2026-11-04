# v0.25.4 release acceptance

PR #347 prevents coordinate replay after uncertain synthesized gesture delivery.
Tap fallback requires an explicit stationary down/up pair without a delayed
start; cancellation and an automatically completed up are excluded. Complex
paths are refused before dispatch when event synthesis is unavailable.

The final PR head passed complete PR checks, Simulator smoke and security
scanning. Local verification passed 274 Swift checks and unsigned iOS compilation.

All five downloaded payload checksums match the release. The app passes strict
signature, notarization and stapled-ticket verification. Embedded/standalone MCP
report 0.25.4 and expose 39 tools. Universal binaries, auxiliary bridge/host tools,
iOS library architectures and six runner source files match their expected
contracts or tag sources. MCP Registry and plugin marketplace report 0.25.4.

The downloaded app is installed, both daemon instances report 0.25.4, and a fresh
installed MCP reads both instances despite an intentionally closed proxy. Local
RunnerActions and RunnerRoutes match the tag. The i13 exit marker is unchanged;
its status remains released, parked for exited, and non-drivable. The default
phone target is preserved.

No phone input, unlock or reconnect was performed. The device-side Runner was
not restarted: the new source is built and loaded on the next explicit connect.
This is artifact and host-upgrade acceptance, not a new physical gesture test.
Machine-readable evidence is in result.json.
