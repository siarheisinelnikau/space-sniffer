#!/usr/bin/env python3
"""Stage an experimental, unsigned macOS app from an already-built Mach-O.

Uses Python 3.11+ standard libraries only. Does not build, run, sign, install,
notarize, archive, or overwrite an application. Header checks are not a macOS
runtime compatibility test.
"""

import argparse
from pathlib import Path
import plistlib
import shutil
import stat
import struct
import tomllib

REPOSITORY = Path(__file__).resolve().parents[1]
TARGET_CPUS = {
    "aarch64-apple-darwin": 0x0100000C,
    "x86_64-apple-darwin": 0x01000007,
}


def package(binary: Path, target: str, output: Path, repository: Path = REPOSITORY) -> Path:
    if target not in TARGET_CPUS:
        raise ValueError(f"Unsupported macOS target: {target}")
    with binary.open("rb") as source:
        header = source.read(32)
    if len(header) < 32:
        raise ValueError("Expected a 64-bit Mach-O executable header.")
    magic, cpu, _, file_type, *_ = struct.unpack("<8I", header)
    if magic != 0xFEEDFACF or cpu != TARGET_CPUS[target] or file_type != 2:
        raise ValueError(f"Expected a thin 64-bit Mach-O executable for {target}.")

    with (repository / "Cargo.toml").open("rb") as source:
        version = tomllib.load(source)["package"]["version"]
    with (repository / "packaging/macos/Info.plist").open("rb") as source:
        metadata = plistlib.load(source)
    metadata["CFBundleShortVersionString"] = version
    metadata["CFBundleVersion"] = version
    license_file = repository / "LICENSE"
    if not license_file.is_file():
        raise FileNotFoundError(license_file)

    app = output / "Space Sniffer.app"
    # No recursive removal or replacement: existing user builds are preserved.
    output.mkdir(parents=True, exist_ok=True)
    app.mkdir()
    contents = app / "Contents"
    executable_dir = contents / "MacOS"
    resources = contents / "Resources"
    executable_dir.mkdir(parents=True)
    resources.mkdir()
    executable = executable_dir / "space-sniffer"
    shutil.copyfile(binary, executable)
    executable.chmod(stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR
                     | stat.S_IRGRP | stat.S_IXGRP | stat.S_IROTH | stat.S_IXOTH)
    shutil.copyfile(license_file, resources / "LICENSE")
    with (contents / "Info.plist").open("wb") as destination:
        plistlib.dump(metadata, destination, sort_keys=False)
    return app


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGET_CPUS, required=True)
    parser.add_argument("--binary", type=Path, help="Defaults to target/TARGET/release/space-sniffer")
    parser.add_argument("--output", type=Path, help="Fresh output directory; existing apps are never replaced")
    args = parser.parse_args()
    binary = args.binary or REPOSITORY / "target" / args.target / "release/space-sniffer"
    output = args.output or REPOSITORY / "target/macos-bundle" / args.target
    try:
        app = package(binary, args.target, output)
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f"Could not prepare macOS app: {error}\n")
    print(f"Unsigned experimental app: {app}")
    print("Manual macOS GUI testing, signing, and notarization are still required.")


if __name__ == "__main__":
    main()
