# Phone-side exit: physical USB validation

On 2026-10-26 the installed, signed **v0.24.1** CLI and daemon were tested on the
operator-authorized **iPhone 13 / iOS 27.0 over USB**, instance `i13`. The existing
instance was upgraded through the official installer. iPhone 17 configuration
was preserved. Stock setup completed successfully; the built source-cache hash
matches the complete packaged Runner source tree from the v0.24.1 release.

The owned fixture was independently confirmed in the foreground. The operator
pressed both physical volume buttons and confirmed within the active observation
window. The fresh Runner log records `Not authorized for performing UI testing
actions`; the supervisor parks and the daemon reports released/phone-ended,
without an owner or actionability. Read-only polling observes **46.13 seconds**
of continued parking without automatic reconnection. No UI text was entered and
no reconnect was requested after the gesture.

The fixture was removed and all ten preexisting apps preserved. Cleanup reported
no errors and left the phone exited. [Sanitized evidence](result.json) excludes
device identifiers, credentials, provisioning material and unrelated phone data.

An initial fixture receipt was not ready on its first read; bounded read-only
waiting corrected that harness issue. An earlier operator-action window expired
without a confirmed gesture. Both are excluded from acceptance. This report
covers the successful, newly opened and confirmed observation window.

Coverage is the physical **USB volume-key exit** on this device/release. The
notification action, passcode cancellation, Wi-Fi interruption, current-app host
variants and older iOS versions remain separate acceptance lanes. This does not
validate the private character-input prototype or promote it into the product.
