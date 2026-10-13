#!/usr/bin/env python3
"""Drive the opted-in SelfRunnerSpike USB mailbox, without starting a XCTest runner."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid

REMOTE_ACTIONS = ["status", "read", "screenshot", "tap", "type", "swipe", "press", "launch", "open_url", "wait_for"]
APP = "dev.iphoneuse.selfrunner.spike"
ACTIONS = ["ddi-mount", "ddi-ticket", "ddi-probe", "tap-offer", "remote-status", "status", "start", "e0", "e1", "e1b", "e1c", "read", "read-runner", "read-delayed", "screenshot", "tap"]


def transfer(device, direction, source, destination):
    subprocess.run([
        "xcrun", "devicectl", "device", "copy", direction, "--device", device,
        "--domain-type", "appDataContainer", "--domain-identifier", APP,
        "--source", str(source), "--destination", str(destination), "--timeout", "15",
    ], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)


def read_status(device, directory):
    path = directory / "status.json"
    path.unlink(missing_ok=True)
    transfer(device, "from", "Documents/research-status.json", path)
    status = json.loads(path.read_text())
    if status.get("version") != 1 or time.time() - status.get("updatedAt", 0) > 10:
        raise RuntimeError("mailbox absent or stale; launch app with --research-mailbox and keep it running")
    uuid.UUID(status["runID"])
    return status


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device", help="required for USB research mailbox")
    parser.add_argument("--remote", action="store_true", help="use paired phone-gateway instead of USB mailbox")
    parser.add_argument("--args", default="{}", help="JSON object for --remote actions")
    parser.add_argument("--expect-run-id", help="refuse a different process")
    parser.add_argument("--no-wait", action="store_true", help="return after USB delivery; not a success assertion")
    parser.add_argument("--wait", type=float, default=95, help="max wait seconds; does not cancel a phone operation")
    parser.add_argument("action", choices=sorted(set(ACTIONS + REMOTE_ACTIONS)))
    args = parser.parse_args()
    if args.remote:
        if args.action not in REMOTE_ACTIONS:
            parser.error("action is not a remote action")
        payload = json.loads(args.args)
        if not isinstance(payload, dict):
            parser.error("--args must be a JSON object")
        return subprocess.run(["phone-gateway", "cmd", args.action, "--args", json.dumps(payload)], check=False).returncode
    if not args.device:
        parser.error("--device is required for USB mailbox")
    if args.action not in ACTIONS or args.args != "{}":
        parser.error("parameterized/phase-two actions require --remote; USB mailbox has no args field")
    if not 1 <= args.wait <= 180:
        parser.error("--wait must be 1..180 seconds")
    with tempfile.TemporaryDirectory(prefix="selfrunner-command-") as folder:
        directory = Path(folder)
        status = read_status(args.device, directory)
        run_id = status["runID"]
        if args.expect_run_id and uuid.UUID(args.expect_run_id) != uuid.UUID(run_id):
            raise RuntimeError("runID changed; refusing command")
        if args.action == "status":
            print(json.dumps(status, ensure_ascii=False, indent=2)); return 0
        if status["phase"] in ("running", "stillRunning"):
            raise RuntimeError("previous command still running; do not enqueue another")
        command_id = str(uuid.uuid4()).upper()
        command = {"version": 1, "id": command_id, "runID": run_id,
                   "expiresAt": time.time() + 60, "action": args.action}
        path = directory / "command.json"
        path.write_text(json.dumps(command))
        transfer(args.device, "to", path, "Documents/research-command.json")
        print(json.dumps({"delivered": True, "id": command_id, "runID": run_id, "action": args.action}), flush=True)
        if args.no_wait:
            return 0
        deadline = time.monotonic() + args.wait
        while time.monotonic() < deadline:
            time.sleep(1)
            status = read_status(args.device, directory)
            if status["runID"] != run_id:
                raise RuntimeError("process changed while waiting; operation result unproven")
            rejection = status.get("rejection", {})
            if rejection.get("id") == command_id:
                print(json.dumps(status, ensure_ascii=False, indent=2)); return 2
            if status.get("id") == command_id and status["phase"] in ("returned", "rejected"):
                # A returned operation may contain an AX/screenshot/tap error. Inspect result.
                print(json.dumps(status, ensure_ascii=False, indent=2))
                return 0 if status["phase"] == "returned" else 2
        raise RuntimeError("wait expired; operation is not cancelled or proven successful; inspect status before retrying")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"research command failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError):
            print(error.stderr.decode(errors="replace"), file=sys.stderr)
        sys.exit(1)
