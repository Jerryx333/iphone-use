#!/usr/bin/env python3
"""Record SDK bootstrap implementation evidence without connecting to a device."""

import argparse
import hashlib
from pathlib import Path
import subprocess
import sys


FUNCTIONS = {
    "PrivateFrameworks/XCTestCore": [
        "-[XCTRunnerDaemonSession initWithConnection:remoteInterfaceCapabilities:]",
        "-[XCTRunnerIDESession _locked_makeTestConfigurationFuture]",
        "___58-[XCTRunnerIDESession _locked_makeTestConfigurationFuture]_block_invoke",
    ],
    "Frameworks/XCUIAutomation": [
        "-[XCTRunnerDaemonSession(XCTRunnerDaemonSessionUIAutomationDelegate) finishInitializationForUIAutomation]",
        "-[XCUIDevice configuredForUITesting]",
        "-[XCUIApplication commonInitWithApplicationSpecifier:device:]",
        "-[XCAXClient_iOS _loadAccessibility:]",
        "-[XCAXClient_iOS loadAccessibility:]",
    ],
    "Frameworks/XCTest": [
        "_XCUIEnableUIAutomation",
        "_XCUIInitializeForUITesting",
    ],
}


def run(*args):
    return subprocess.run(
        args, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        encoding="utf-8", errors="replace",
    ).stdout


def selected_functions(disassembly, names):
    """Keep complete symbol bodies; addresses are observations, never lookup keys."""
    selected = {}
    current = None
    for line in disassembly.splitlines():
        if line and not line[0].isspace() and line.endswith(":"):
            current = line[:-1] if line[:-1] in names else None
            if current is not None:
                selected[current] = []
        elif current is not None:
            selected[current].append(line)
    return selected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True,
                        help="new Markdown output file; existing files are refused")
    parser.add_argument("--developer-dir", type=Path,
                        help="Xcode Developer directory (default: xcode-select -p)")
    args = parser.parse_args()
    if args.out.exists():
        parser.error(f"output already exists: {args.out}")
    developer = args.developer_dir or Path(run("xcode-select", "-p").strip())
    objdump = developer / "Toolchains/XcodeDefault.xctoolchain/usr/bin/llvm-objdump"
    dwarf = developer / "Toolchains/XcodeDefault.xctoolchain/usr/bin/dwarfdump"
    version = run(str(developer / "usr/bin/xcodebuild"), "-version").strip()
    sections = [
        "# XCTest bootstrap SDK evidence",
        f"Toolchain: `{version.replace(chr(10), '; ')}`",
        "Read-only iPhoneOS SDK arm64 disassembly. No device, pairing file or runtime "
        "trace is accessed. This does not establish behavior of an iOS 27 personalized DDI.",
    ]
    missing = []
    for relative, names in FUNCTIONS.items():
        name = relative.split("/")[-1]
        binary = developer / "Platforms/iPhoneOS.platform/Developer/Library" / (
            relative + ".framework"
        ) / name
        if not binary.is_file():
            raise FileNotFoundError(binary)
        digest = hashlib.sha256(binary.read_bytes()).hexdigest()
        uuids = run(str(dwarf), "--uuid", str(binary)).strip()
        sections.extend([
            f"## {name}", f"Binary: `{binary}`", f"SHA256: `{digest}`",
            f"```text\n{uuids}\n```",
        ])
        bodies = selected_functions(run(
            str(objdump), "--macho", "--arch=arm64", "--disassemble",
            "--symbolize-operands", str(binary),
        ), names)
        for symbol in names:
            sections.append(f"### `{symbol}`")
            body = bodies.get(symbol)
            if not body:
                missing.append(symbol)
                sections.append("Symbol not found; do not infer that the runtime method is absent.")
            else:
                sections.append("```text\n" + symbol + ":\n" + "\n".join(body) + "\n```")
    # Exclusive creation also protects against another process creating the file during collection.
    with args.out.open("x", encoding="utf-8") as output:
        output.write("\n\n".join(sections) + "\n")
    print(f"Evidence written: {args.out}")
    if missing:
        print("Missing SDK symbols:\n" + "\n".join(missing), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        sys.exit(1)
