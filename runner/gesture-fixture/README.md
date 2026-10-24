# Gesture fixture

A simulator-only UIKit receiver for `scripts/test-drag-simulator.py`. It records
actual touch events in the `gesture-result` accessibility label. It never invents
a success from a transport acknowledgement and never resets its counters on a
second press; every case must relaunch with a new nonce.

Use a disposable simulator and a daemon already bound to its exact UDID:

```sh
python3 scripts/test-drag-simulator.py \
  --base http://127.0.0.1:44331 --udid <simulator-uuid> \
  --runner-record <instance-state-dir>/simulator.json \
  --mcp <candidate-iphone-use-mcp> --out <new-artifacts-directory>
```

The harness compiles and installs this ad-hoc-signed app (no Apple account required), then verifies
long press, the native swipe compatibility route, single/batch/Flow waypoint
paths and a slow-drift negative control. The optional `--settings-suite` output
must lie inside the artifact directory; it binds a localized app-label assertion
to the independently observed Settings bundle ID and source PID.

`test-simulator-smoke.sh` runs the harness in CI. Results, screenshots, source
hashes and the candidate MCP binary digest are retained in the artifact. The
caller owns simulator lifecycle and should stop/delete its disposable device.
This is not a real-phone folder-page acceptance test.

`--input-readiness` adds a standard UIKit table and search controller. The
fixture records actual row selections, scroll offsets, editing callbacks and
received text. It checks a settled tap, three scroll-then-tap batches, and three
first-focus typing batches. Each case starts with a new nonce and sends once;
an acknowledgement without the matching UIKit result fails. Simulator smoke
runs these cases in addition to the six gesture cases above.
