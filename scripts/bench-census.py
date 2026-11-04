#!/usr/bin/env python3
"""Latency census of iphone-use primitives on a real phone — the shared yardstick.

Run before and after a performance change, on the same screen, and paste both
tables into the PR. It goes through the daemon only (never runner :8100 directly,
which bypasses the owner lease and is not counted as activity), takes the
phone with an owner name and a hold, and releases both at the end.

    IPHONE_USE_URL=http://127.0.0.1:45432 IPHONE_USE_TOKEN=... \\
        python3 scripts/bench-census.py [--runs 3] [--app com.apple.calculator]

Each row: wall-clock ms per run, then the daemon's own timing split of the last
run (daemon vs runner, per runner call). Taps use Calculator's keypad identifiers, so
keep --app at the default for comparable numbers.
"""

import argparse
import http.client
import json
import os
import sys
import time
import urllib.error
import urllib.request
import uuid


class CensusError(RuntimeError):
    """The current sample cannot justify another phone action."""


# Installs made before the rename set PHONE_REMOTE_*; read them as IPHONE_USE_*
# (the new name wins).
for _key, _value in list(os.environ.items()):
    if _key.startswith("PHONE_REMOTE_") and len(_key) > len("PHONE_REMOTE_"):
        os.environ.setdefault("IPHONE_USE_" + _key[len("PHONE_REMOTE_"):], _value)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--app", default="com.apple.calculator")
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be at least 1")
    owner = "bench-census-" + uuid.uuid4().hex
    base = os.environ.get("IPHONE_USE_URL", "http://127.0.0.1:44321").rstrip("/")
    token = os.environ.get("IPHONE_USE_TOKEN", "")
    headers = {
        "Authorization": "Bearer " + token,
        "X-Phone-Control": "1",
        "X-Phone-Owner": owner,
        "Content-Type": "application/json",
    }

    def request(method, path, body=None):
        started = time.monotonic()
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(base + path, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=90) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as error:
            raise CensusError(f"{method} {path}: HTTP {error.code}; stopped without replay") from None
        except (OSError, http.client.HTTPException) as error:
            raise CensusError(f"{method} {path}: {type(error).__name__}; outcome unconfirmed, stopped without replay") from None
        wall = (time.monotonic() - started) * 1000
        if path == "/agent/screenshot" and raw.startswith((b"\x89PNG\r\n\x1a\n", b"\xff\xd8\xff")):
            return wall, {"image_bytes": len(raw)}
        try:
            parsed = json.loads(raw)
        except (ValueError, UnicodeDecodeError):
            raise CensusError(f"{method} {path}: unreadable response; stopped without replay") from None
        if not isinstance(parsed, dict) or not parsed:
            raise CensusError(f"{method} {path}: missing response object; stopped without replay")
        if path == "/agent/elements" and (
            not isinstance(parsed.get("elements"), list)
            or not isinstance(parsed.get("snapshot"), str)
            or not parsed["snapshot"].strip()
        ):
            raise CensusError(f"{method} {path}: missing confirmed tree; stopped without replay")
        if path == "/agent/screenshot":
            raise CensusError(f"{method} {path}: response is not an image; stopped without replay")
        if parsed.get("ok") is False or parsed.get("error") or parsed.get("outcome") == "unknown":
            raise CensusError(f"{method} {path}: unsuccessful or unknown outcome; stopped without replay")
        if method == "POST" and parsed.get("ok") is not True:
            raise CensusError(f"{method} {path}: missing acknowledgement; stopped without replay")
        return wall, parsed

    def act(action):
        return request("POST", "/agent/input", action)

    status = request("GET", "/agent/status")[1]
    if status.get("owner") is not None:
        print(f"phone is in use by {status.get('owner')!r}; not benchmarking", file=sys.stderr)
        return 1
    if status.get("drivable") is not True:
        print(f"phone is not drivable ({status.get('device_state')}): {status.get('hint')}", file=sys.stderr)
        return 1
    print(f"daemon {status.get('version')} · transport {status.get('transport', '?')} · runs {args.runs}")

    ops = [
        ("status", lambda: request("GET", "/agent/status"), False),
        ("launch_app (running)", lambda: act({"type": "launch_app", "bundle": args.app}), False),
        ("tap x,y", lambda: act({"type": "tap", "x": 0.5, "y": 0.95}), True),
        ("tap_locator identifier", lambda: act({"type": "tap_locator", "locator": {"identifier": "Seven"}}), True),
        ("tap label", lambda: act({"type": "tap", "label": "7"}), True),
        ("key return", lambda: act({"type": "key", "name": "return"}), True),
        # A tap straight after a read (an agent that just looked, a flow whose
        # wait_for just matched) may reuse that read; only the tap is timed.
        ("read → tap label", lambda: (request("GET", "/agent/elements"), act({"type": "tap", "label": "7"}))[1], True),
        ("read → tap_locator", lambda: (request("GET", "/agent/elements"), act({"type": "tap_locator", "locator": {"identifier": "Seven"}}))[1], True),
        ("elements (full tree)", lambda: request("GET", "/agent/elements"), True),
        ("screenshot", lambda: request("GET", "/agent/screenshot"), True),
        ("swipe", lambda: act({"type": "swipe", "x1": 0.5, "y1": 0.6, "x2": 0.5, "y2": 0.4}), True),
        ("shortcut home", lambda: act({"type": "shortcut", "name": "home"}), False),
    ]
    failed = False
    held = False
    try:
        request("POST", "/agent/hold", {"secs": 900})
        held = True
        act({"type": "launch_app", "bundle": args.app})  # warm-up (session, auto DND)
        time.sleep(1)
        for name, op, needs_app in ops:
            if needs_app:
                act({"type": "launch_app", "bundle": args.app})
            walls, last = [], {}
            for _ in range(args.runs):
                wall, last = op()
                walls.append(wall)
            timing = last.get("timing") or {}
            calls = ", ".join(f"{c['call']}×{c.get('count', 1)}={c['ms']}" for c in timing.get("runner", []))
            error = f" ERR {last.get('error')}" if last.get("ok") is False else ""
            print(
                f"{name:24s} wall ms: {', '.join(f'{w:.0f}' for w in walls):18s} "
                f"| daemon={timing.get('daemon_ms')} runner={timing.get('runner_ms')} [{calls}]{error}",
                flush=True,
            )
    except CensusError as error:
        print(str(error), file=sys.stderr)
        failed = True
    finally:
        # A refused/unacknowledged hold must not be followed by another claim.
        # Owner release itself never claims a lease, and is scoped to this run.
        cleanup = ([('/agent/hold', {"secs": 0})] if held else []) + [('/agent/owner', {"release": True})]
        for path, body in cleanup:
            try:
                request("POST", path, body)
            except CensusError as error:
                print(f"cleanup: {error}", file=sys.stderr)
                failed = True
    print("Primitive timing census only; task postconditions were not independently verified.")
    return 1 if failed else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except CensusError as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1)
