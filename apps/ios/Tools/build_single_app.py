#!/usr/bin/env python3
"""Build a unified candidate. Produces one installable app, never installs it."""
import argparse
import os
import plistlib
import subprocess
from pathlib import Path
from package_single_app import assemble, sign

REPO = Path(__file__).resolve().parents[3]


def run(command, log):
    log.parent.mkdir(parents=True, exist_ok=True)
    with os.fdopen(os.open(log, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as output:
        result = subprocess.run(command, cwd=REPO, stdout=output, stderr=subprocess.STDOUT)
    if result.returncode:
        for line in log.read_text(errors="replace").splitlines():
            if "error:" in line:
                print(line)
        raise SystemExit("Build failed; private log: " + str(log))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--host-lib", type=Path, required=True)
    parser.add_argument("--embed-lib", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    for library in (args.host_lib, args.embed_lib):
        if not library.is_file():
            raise SystemExit("Required native library missing")
    if not (REPO / "experiments/phone-self-host/Resources/DeveloperImage/BuildManifest.plist").is_file():
        raise SystemExit("Prepare private developer image resources first")
    profile = Path.home() / "Library/LaunchAgents/com.leeguoo.iphone-use.wda.plist"
    env = plistlib.loads(profile.read_bytes()).get("EnvironmentVariables", {}) if profile.exists() else {}
    auth = ["-allowProvisioningUpdates"]
    for key, flag in [("WDA_ASC_KEY_PATH", "-authenticationKeyPath"),
                      ("WDA_ASC_KEY_ID", "-authenticationKeyID"),
                      ("WDA_ASC_ISSUER_ID", "-authenticationKeyIssuerID")]:
        value = os.environ.get(key) or env.get(key)
        if value:
            auth.extend([flag, value])
    build = REPO / "apps/ios/build-single-app"
    subprocess.run(["xcodegen", "generate", "--spec", "apps/ios/project.yml"], cwd=REPO, check=True)
    host_flags = '-force_load "' + str(args.host_lib.resolve()) + '" -framework Security'
    host_flags += " " + " ".join("-u _" + name for name in ["ipu_host_start", "ipu_host_stop", "ipu_host_status", "ipu_host_string_free", "ipu_host_copy_pair_record"])
    common = ["xcodebuild", "-configuration", "Debug", "-destination", "generic/platform=iOS", "-jobs", "2"]
    run(common + ["-project", "apps/ios/IPhoneUseRemote.xcodeproj", "-scheme", "IPhoneUseRemote",
                  "-derivedDataPath", str(build / "main"), "IPU_HOST_LDFLAGS=" + host_flags] + auth + ["build"], build / "main-build.log")
    embed_flags = '$(inherited) -force_load "' + str(args.embed_lib.resolve()) + '" -framework Security -framework SystemConfiguration'
    embed_flags += " " + " ".join("-u _" + name for name in ["ipu_daemon_start", "ipu_daemon_state", "ipu_daemon_error", "ipu_daemon_pair_code"])
    run(common + ["-project", "runner/IPhoneUseRunner/IPhoneUseRunner.xcodeproj", "-scheme", "IPhoneUseRunner",
                  "-derivedDataPath", str(build / "control"), "PRODUCT_NAME=PhoneUse",
                  "OTHER_LDFLAGS=" + embed_flags] + auth + ["build-for-testing"], build / "control-build.log")
    main = build / "main/Build/Products/Debug-iphoneos/Phone Use Remote.app"
    runner = build / "control/Build/Products/Debug-iphoneos/PhoneUse-Runner.app"
    candidate = assemble(main, runner, args.output.resolve())
    sign(candidate, main)
    print("Single-app signed candidate ready (device acceptance pending): " + str(candidate))
