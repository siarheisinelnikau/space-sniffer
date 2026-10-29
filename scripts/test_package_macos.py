"""Bundle layout and input checks; these tests never execute a macOS binary."""

import os
from pathlib import Path
import plistlib
import struct
import tempfile
import tomllib
import unittest

from package_macos import REPOSITORY, TARGET_CPUS, package


class BundleTests(unittest.TestCase):
    def test_both_architectures_have_matching_metadata_license_and_executable(self):
        with (REPOSITORY / "Cargo.toml").open("rb") as source:
            version = tomllib.load(source)["package"]["version"]
        with tempfile.TemporaryDirectory(prefix="space-sniffer-bundle-test-") as temporary:
            root = Path(temporary)
            for target, cpu in TARGET_CPUS.items():
                with self.subTest(target=target):
                    binary = root / f"binary-{target}"
                    binary.write_bytes(struct.pack("<8I", 0xFEEDFACF, cpu, 0, 2, 0, 0, 0, 0))
                    app = package(binary, target, root / target)
                    with (app / "Contents/Info.plist").open("rb") as source:
                        metadata = plistlib.load(source)
                    executable = app / "Contents/MacOS" / metadata["CFBundleExecutable"]
                    self.assertEqual(executable.read_bytes(), binary.read_bytes())
                    self.assertEqual(metadata["CFBundleShortVersionString"], version)
                    self.assertEqual(metadata["CFBundleVersion"], version)
                    self.assertEqual(metadata["CFBundlePackageType"], "APPL")
                    self.assertEqual(metadata["CFBundleDevelopmentRegion"], "en")
                    self.assertEqual((app / "Contents/Resources/LICENSE").read_bytes(), (REPOSITORY / "LICENSE").read_bytes())
                    if os.name != "nt":
                        self.assertEqual(executable.stat().st_mode & 0o777, 0o755)

    def test_rejects_wrong_architecture_and_non_macos_binaries_before_creating_output(self):
        with tempfile.TemporaryDirectory(prefix="space-sniffer-bundle-test-") as temporary:
            root = Path(temporary)
            binary = root / "input"
            for payload in [b"MZ" + b"\0" * 30, struct.pack("<8I", 0xFEEDFACF, TARGET_CPUS["x86_64-apple-darwin"], 0, 2, 0, 0, 0, 0)]:
                binary.write_bytes(payload)
                with self.assertRaises(ValueError):
                    package(binary, "aarch64-apple-darwin", root / "output")
                self.assertFalse((root / "output").exists())

    def test_never_replaces_an_existing_app(self):
        with tempfile.TemporaryDirectory(prefix="space-sniffer-bundle-test-") as temporary:
            root = Path(temporary)
            binary = root / "input"
            binary.write_bytes(struct.pack("<8I", 0xFEEDFACF, TARGET_CPUS["aarch64-apple-darwin"], 0, 2, 0, 0, 0, 0))
            existing = root / "output/Space Sniffer.app"
            existing.mkdir(parents=True)
            sentinel = existing / "user-build"
            sentinel.write_text("keep this build", encoding="utf-8")
            with self.assertRaises(FileExistsError):
                package(binary, "aarch64-apple-darwin", root / "output")
            self.assertEqual(sentinel.read_text(encoding="utf-8"), "keep this build")


if __name__ == "__main__":
    unittest.main()
