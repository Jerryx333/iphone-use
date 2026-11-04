# v0.25.3 release acceptance

The published release distinguishes session invalidation from recorded on-phone
exit while keeping both parked until an explicit request. Unknown markers also
remain parked; prewarming and automatic schedules cannot resume them.

Downloaded release verification covered all five payload checksums, app signing,
notarization and stapled ticket, universal architectures, CLI version, embedded
and standalone MCP discovery (39 tools), and runner sources matching the tag.
The MCP Registry and plugin marketplace both report 0.25.3.

The downloaded app is installed locally. Both default and i13 daemon APIs report
0.25.3, and a fresh installed stdio MCP reads both instances with an intentionally
closed proxy endpoint. The i13 exit marker is byte-for-byte unchanged, status
remains released and non-drivable, and the default phone target is unchanged.

No phone input, unlock, reconnect or new physical acceptance was performed for
this release. The prior volume-button acceptance remains separate from this
artifact and upgrade verification. Machine-readable evidence is in result.json.
