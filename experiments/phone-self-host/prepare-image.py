#!/usr/bin/env python3
"""Copy the user's installed Apple DDI into a private local build, never into git."""
import os
from pathlib import Path
import plistlib
import shutil

ROOT = Path(__file__).resolve().parent
source = Path(os.environ.get("IPU_DDI_RESTORE", "/Library/Developer/DeveloperDiskImages/iOS_DDI/Restore"))
if not (source/"BuildManifest.plist").is_file():
    raise SystemExit("Apple developer image not installed; set IPU_DDI_RESTORE to its Restore directory")
manifest = plistlib.loads((source/"BuildManifest.plist").read_bytes())
paths = {"BuildManifest.plist"}
for identity in manifest.get("BuildIdentities", []):
    for name in ["PersonalizedDMG", "LoadableTrustCache"]:
        path = identity.get("Manifest", {}).get(name, {}).get("Info", {}).get("Path")
        if path:
            relative = Path(path)
            if relative.is_absolute() or ".." in relative.parts:
                raise SystemExit("Invalid developer image path")
            paths.add(path)
output = ROOT/"Resources/DeveloperImage"
output.mkdir(parents=True, exist_ok=True)
for name in sorted(paths):
    src = source/name
    if not src.is_file(): raise SystemExit("Developer image component missing")
    dst = output/name
    dst.parent.mkdir(parents=True, exist_ok=True)
    if not dst.exists() or src.stat().st_size != dst.stat().st_size or src.stat().st_mtime > dst.stat().st_mtime:
        shutil.copy2(src, dst)
print("Apple developer image prepared for local build only")
