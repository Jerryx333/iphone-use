#!/usr/bin/env python3
"""Print the request-signing token of the Self Runner's in-app runner API (RunnerCore.bundle).

The app derives it from the phone-gateway pairing PSK (spikes/self-runner-app/Sources/
RemotePairing.swift, runnerToken): hex(HMAC-SHA256(PSK, "selfrunner-runner-token-v1")). This
computes the same value from the gateway's copy of the PSK, so the token never crosses the network.

    IPU_RUNNER_TOKEN=$(python3 scripts/selfrunner-runner-token.py) \\
        python3 scripts/runner-compat.py --base http://<phone>:8100 --mjpeg http://<phone>:9100

The PSK is read from $PHONE_GATEWAY_HOME/psk (default ~/.config/phone-gateway/psk).
"""

import base64
import hashlib
import hmac
import os
import sys

LABEL = b"selfrunner-runner-token-v1"


def main():
    home = os.environ.get("PHONE_GATEWAY_HOME") or os.path.expanduser("~/.config/phone-gateway")
    try:
        with open(os.path.join(home, "psk"), encoding="ascii") as handle:
            psk = base64.b64decode(handle.read().strip(), validate=True)
    except (OSError, ValueError) as error:
        sys.exit(f"selfrunner-runner-token: cannot read the gateway PSK: {error}")
    if len(psk) != 32:
        sys.exit("selfrunner-runner-token: the gateway PSK is not 32 bytes")
    print(hmac.new(psk, LABEL, hashlib.sha256).hexdigest())


if __name__ == "__main__":
    main()
