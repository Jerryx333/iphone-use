# Remote transport checks (macOS)

Run `bash spikes/self-runner-app/tests/remote-transport-check.sh`. It compiles all four
remote Swift sources using the macOS SDK, checks §15.4 vectors and malformed records,
and validates new and legacy pairings. It neither accesses Keychain nor imports a pairing.

Optional integration requires a gateway configured with **disposable test credentials**:
PSK bytes `00..1f` (base64 `AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=`),
a matching test device ID, WS loopback listener, and runner forwarding. Never point
`PAIRING_FILE` at a production pairing. The harness rejects any other PSK.

```sh
GATEWAY_WS_URL=ws://127.0.0.1:7383 \
PAIRING_FILE=/path/to/disposable-vector-pair.json \
GATEWAY_FORWARD_PORT=18100 \
bash spikes/self-runner-app/tests/remote-transport-check.sh
```

Start the test gateway with `--ws-bind 127.0.0.1:7383 --runner-forward 127.0.0.1:18100`
and its disposable pairing (plus the gateway's other required options).
`GATEWAY_FORWARD_PORT` defaults to 18100. The test creates an ephemeral local echo
listener, maps the tunnel's port 8100 to it, waits for control welcome, echoes over
1 MiB through open/opened, half-closes the forward socket, and verifies the complete
reply and EOF. It also waits for one gateway ping and sends pong. The 55-second
whole-test deadline allows the normal gateway heartbeat interval.

Both opt-in variables must be set; otherwise integration prints `skipped`.
Integration needs network and local-listener permission. Run it in the main session
with `--net` when those operations are denied by the worktree sandbox.

Device-only checks remain: iOS build with libself_boot/DDI, system CA handling through
Cloudflare, Wi-Fi/cellular/VPN path transitions, background survival, route display,
and reconnect after reinstall. Network.framework bounds WebSocket message assembly
via `maximumMessageSize`; fragmented/oversize messages and independent automatic
Ping/Pong under byte-stream backpressure should also be exercised against the real gateway.

Known harness limitation (2026-10-14): the in-process forward client is a macOS `NWConnection`;
after both FINs it sometimes reports `ENETDOWN` and drops the tail of the echo, on the LAN and the
WSS route alike, while the gateway log shows every byte written with `code="eof"`. A plain BSD-socket
client never loses data. For a reliable forward check, keep the tunnels up with
`FORWARD_EXTERNAL=<seconds>` and drive port `GATEWAY_FORWARD_PORT` from another client
(e.g. Python: send ~1 MiB, `shutdown(SHUT_WR)`, read to EOF, compare).
