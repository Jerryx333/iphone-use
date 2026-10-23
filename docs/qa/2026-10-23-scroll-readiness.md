# Scroll followed by a tap: UIKit acceptance

An installed v0.22.2 on iPhone 17 Pro Max reproduced a Settings batch that
acknowledged a scroll and a tap on General but stayed on the root screen. A
settled tap opened General. The original intermittent first-focus text loss
was not reproduced in three fresh physical search trials.

A disposable UIKit table on Mac 190 reproduced the scroll/tap failure in
three fresh launches. Every batch returned success, but the table delegate
recorded no selection. The table moved to offset 700. Replacing the gesture
with a slow final movement selected row 8 in all three control trials.

The fix applies a slow final movement to coordinate and element-contained
`scroll` actions. The last at most 30 points take 250 ms, limiting release
speed to 120 pt/s. The table now stops at offset 440 and selects row 8 once
in each of three identical scroll-then-tap batches.

Validation used Xcode 27.0 and iOS Simulator 26.2. All 13 OS-delivered cases
passed: the six existing gesture cases, a settled row tap, three scroll/tap
batches and three first-focus Chinese text batches. The fixture records
UIKit delegate callbacks and received text, starts each case with a fresh
nonce, and sends each batch once. Its verdict rejects an acknowledgement
without the matching row selection, stale receipts, duplicate selections,
and invalid scroll offsets. The 14 verdict checks and 874 server unit tests
passed; one existing unit test remained ignored.

The anonymized [receipt](2026-10-23-scroll-readiness.json) binds the tested
candidate binary to hashes of the changed input and fixture sources. The
baseline was a development CLI, identified separately by its SHA-256.
Candidate acceptance in physical Settings is pending.
