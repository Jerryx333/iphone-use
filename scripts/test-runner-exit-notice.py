#!/usr/bin/env python3
"""Exercise production notification callbacks with a deterministic notification-center double."""
from pathlib import Path
import subprocess
import tempfile
root = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="ipu-exit-notice-") as scratch:
    out = Path(scratch)
    source = (root / "runner/IPhoneUseRunner/IPhoneUseRunnerUITests/RunnerExitNotice.swift").read_text()
    (out / "RunnerExitNotice.swift").write_text(source.replace("import UserNotifications", ""))
    (out / "main.swift").write_text((root / "runner/unit-check/exit-notice-main.swift").read_text())
    subprocess.run(["xcrun", "swiftc", "-swift-version", "5",
                    str(root / "runner/unit-check/exit-notice-stub.swift"),
                    str(out / "RunnerExitNotice.swift"), str(out / "main.swift"),
                    "-o", str(out / "check")], check=True)
    subprocess.run([str(out / "check")], check=True, timeout=10)
