# v0.25.5 release acceptance

PR #348 fences standalone screenshots, element reads, settled-frame caches and
MJPEG/H.264 publication by privacy generation. Old reads, subscriptions and
encoder callbacks cannot publish through a private on/off transition. Cleaning
an expired deadline preserves work started after expiry. Failed, empty or
interrupted captures and host RPC failures clear the panel's previous pixels.
Action-attached observations retain their separate action receipt contract;
bytes already committed to the connection cannot be recalled.

The baseline CI failed all six privacy race cases. The final candidate passed
all seven cases, including expired-hold normalization, complete PR checks and
real stdio MCP coverage for rejection, empty and truncated responses. The real
panel script verifies old-image removal and preserved failure guidance. Simulator
smoke passed on the same backend/Runner code; subsequent changes were panel and
test-fixture changes only.

The five downloaded payload checksums, app signature/notarization/stapled ticket,
universal binaries, auxiliary tools and iOS library architectures pass independent
verification. Embedded and standalone MCP report 0.25.5 with 39 tools; six Runner
source files match the tag. Registry and plugin marketplace report 0.25.5.

The downloaded app and both local daemons report 0.25.5. A fresh installed MCP
reads both instances with an intentionally closed proxy. The i13 exit marker is
unchanged, its status remains parked for exited and non-drivable, and the default
phone target is preserved. No phone input, unlock, reconnect or new physical
acceptance was performed. Machine-readable evidence is in result.json.
