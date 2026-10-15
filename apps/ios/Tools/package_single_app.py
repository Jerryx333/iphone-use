#!/usr/bin/env python3
"""Assemble one application; never install a second runner application.

The main app must already contain its own packet-tunnel extension and bootstrap.
Only the control .xctest and its SDK frameworks are copied from the runner product.
This validates/signs packaging, not runtime permission or device acceptance.
"""
import argparse
import hashlib
import plistlib
import shutil
import subprocess
import tempfile
from pathlib import Path


def plist(path):
    return plistlib.loads(path.read_bytes())


DEFAULT_BUNDLE_ID = "com.leeguoo.iphone-use.remote"


def validate_main(app, bundle_id=DEFAULT_BUNDLE_ID):
    info = plist(app / "Info.plist")
    if info.get("CFBundleIdentifier") != bundle_id:
        raise ValueError("Unexpected main application identity")
    if not (app / info["CFBundleExecutable"]).is_file():
        raise ValueError("Main executable missing")
    tunnels = []
    for extension in (app / "PlugIns").glob("*.appex"):
        data = plist(extension / "Info.plist")
        if data.get("NSExtension", {}).get("NSExtensionPointIdentifier") == "com.apple.networkextension.packet-tunnel":
            tunnels.append(extension)
    if len(tunnels) != 1:
        raise ValueError("Single-app product requires exactly one bundled local tunnel")
    return info


def assemble(main, runner, output, bundle_id=DEFAULT_BUNDLE_ID):
    info = validate_main(main, bundle_id)
    if output.exists():
        raise ValueError("Output already exists; use a new output path")
    if not (runner / "PlugIns/PhoneUse.xctest").is_dir():
        raise ValueError("Build the control bundle with PRODUCT_NAME=PhoneUse")
    shutil.copytree(main, output, symlinks=True)
    # The local launcher's test configuration derives PhoneUse.xctest from
    # this executable name. Keep the main executable, UI, identity and data.
    old = output / info["CFBundleExecutable"]
    old.rename(output / "PhoneUse-Runner")
    info["CFBundleExecutable"] = "PhoneUse-Runner"
    info["CFBundleDisplayName"] = "Phone Use"
    info["IPUSingleAppControl"] = True
    (output / "Info.plist").write_bytes(plistlib.dumps(info))
    plugins = output / "PlugIns"
    for existing in plugins.glob("*.xctest"):
        shutil.rmtree(existing)
    shutil.copytree(runner / "PlugIns/PhoneUse.xctest", plugins / "PhoneUse.xctest", symlinks=True)
    frameworks = output / "Frameworks"
    frameworks.mkdir(exist_ok=True)
    for item in (runner / "Frameworks").iterdir() if (runner / "Frameworks").exists() else []:
        destination = frameworks / item.name
        if destination.exists():
            # Conflicting frameworks must be reviewed, never silently replaced.
            if item.is_file() and destination.is_file() and item.read_bytes() == destination.read_bytes():
                continue
            raise ValueError("Conflicting framework: " + item.name)
        if item.is_dir():
            shutil.copytree(item, destination, symlinks=True)
        else:
            shutil.copy2(item, destination)
    if any(output.rglob("*.app")):
        raise ValueError("Nested runner applications are not a single-app product")
    return output


def sign(app, original):
    entitlements = subprocess.run(["codesign", "-d", "--entitlements", ":-", str(original)],
                                 check=True, capture_output=True).stdout
    with tempfile.TemporaryDirectory(prefix="ipu-single-app-sign-") as directory:
        prefix = Path(directory) / "certificate"
        subprocess.run(["codesign", "-d", "--extract-certificates=" + str(prefix), str(original)],
                       check=True, capture_output=True)
        # A provisioning profile may list several developers. Use the exact
        # leaf certificate that signed the original app, not its first entry.
        identity = hashlib.sha1(Path(str(prefix) + "0").read_bytes()).hexdigest().upper()
        path = Path(directory) / "entitlements.plist"
        path.write_bytes(entitlements)
        path.chmod(0o600)
        # Extensions retain their separately signed identities. Re-sign only
        # copied test code/frameworks and then seal the main application.
        items = list((app / "Frameworks").glob("*.framework"))
        items += list((app / "Frameworks").glob("*.dylib"))
        items += [app / "PlugIns/PhoneUse.xctest"]
        for item in items:
            subprocess.run(["codesign", "--force", "--sign", identity, "--timestamp=none", str(item)],
                           check=True, capture_output=True)
        subprocess.run(["codesign", "--force", "--sign", identity, "--timestamp=none",
                        "--entitlements", str(path), "--generate-entitlement-der", str(app)],
                       check=True, capture_output=True)
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--main", required=True, type=Path)
    parser.add_argument("--runner", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--bundle-id", default=DEFAULT_BUNDLE_ID)
    args = parser.parse_args()
    candidate = assemble(args.main.resolve(), args.runner.resolve(), args.output.resolve(), args.bundle_id)
    sign(candidate, args.main.resolve())
    print("One signed application assembled; physical single-app acceptance still required: " + str(candidate))
