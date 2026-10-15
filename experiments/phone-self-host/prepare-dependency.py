#!/usr/bin/env python3
"""Materialize a pinned MIT dependency and apply the project's scoped patch."""
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parent
REVISION = "41c388baef0dd55c0ce4924789abbfde889d9b87"
PATCH = ROOT / "patches/idevice-rsd-xctest.patch"
OUTPUT = ROOT / ".deps/idevice"
stamp = {"revision": REVISION, "patch_sha256": hashlib.sha256(PATCH.read_bytes()).hexdigest()}
marker = OUTPUT / ".ipu-source.json"
if marker.exists() and json.loads(marker.read_text()) == stamp:
    print("Pinned RSD dependency ready")
    raise SystemExit(0)
if OUTPUT.exists():
    if not marker.exists():
        raise SystemExit("Refusing to replace an unowned dependency directory")
    shutil.rmtree(OUTPUT)
OUTPUT.mkdir(parents=True)
# An explicitly provided local checkout must be the exact pinned revision.
source = os.environ.get("IPU_IDEVICE_SOURCE")
if source:
    source = Path(source).resolve()
    revision = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    clean = subprocess.run(["git", "-C", str(source), "diff", "--quiet", "HEAD", "--", "idevice"]).returncode == 0
    if not clean:
        raise SystemExit("Local dependency source has uncommitted edits")
    if revision != REVISION:
        raise SystemExit("Local source revision does not match pin")
    shutil.copytree(source / "idevice", OUTPUT / "idevice", ignore=shutil.ignore_patterns("target"))
    shutil.copy2(source / "LICENSE.txt", OUTPUT / "LICENSE.txt")
else:
    url = f"https://codeload.github.com/leeguooooo/idevice/tar.gz/{REVISION}"
    data = urllib.request.urlopen(url, timeout=60).read()
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive.getmembers():
            parts = Path(member.name).parts[1:]
            if not parts or parts[0] not in ("idevice", "LICENSE.txt"):
                continue
            if member.isdir():
                continue
            # The pinned repo's package README links to the root README. It is
            # documentation only; never materialize archive symlinks.
            if member.issym() and parts == ("idevice", "README.md") and member.linkname == "../README.md":
                continue
            if not member.isfile() or any(p in ("..", "") for p in parts):
                raise SystemExit("Invalid archive entry")
            destination = OUTPUT.joinpath(*parts)
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(archive.extractfile(member).read())
subprocess.run(["git", "apply", "--directory", str(OUTPUT.relative_to(ROOT)), str(PATCH)], cwd=ROOT, check=True)
marker.write_text(json.dumps(stamp, indent=2) + "\n")
print("Pinned RSD dependency prepared")
