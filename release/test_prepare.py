"""Inert fixtures for release validation; no applications, devices or GitHub requests."""
import hashlib
import json
import os
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import prepare


class ReleasePreparationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "release/notes").mkdir(parents=True)
        (self.root / "release/config.json").write_text(json.dumps({
            "repository": "owner/test", "product": "Test", "kind": "desktop",
            "assets": [{"name": "Test-v{version}.exe"}], "notices": False,
        }), encoding="utf-8")
        (self.root / "Cargo.toml").write_text('[package]\nname="test"\nversion="1.2.3"\n', encoding="utf-8")
        (self.root / "release/notes/1.2.3.md").write_text("# Test 1.2.3\n" + "Verified release improvement. " * 8, encoding="utf-8")
        self.inputs = self.root / "input"
        self.inputs.mkdir()
        self.exe = self.inputs / "Test-v1.2.3.exe"
        header = bytearray(256)
        header[:2] = b"MZ"
        struct.pack_into("<I", header, 60, 128)
        header[128:134] = b"PE\0\0\x64\x86"
        self.exe.write_bytes(header)
        self.environ = patch.dict(os.environ, {"GITHUB_REPOSITORY": "owner/test", "GITHUB_SHA": "a" * 40})
        self.environ.start()
        self.addCleanup(self.environ.stop)

    def test_stages_architecture_checked_executable_and_exact_hash(self):
        result = prepare.stage(self.root, self.inputs, self.root / "out")
        self.assertEqual(result["commit"], "a" * 40)
        self.assertEqual(result["assets"][0]["digest"], "sha256:" + hashlib.sha256(self.exe.read_bytes()).hexdigest())

    def test_wrong_architecture_is_rejected_before_staging(self):
        data = bytearray(self.exe.read_bytes())
        data[132:134] = b"\x4c\x01"
        self.exe.write_bytes(data)
        with self.assertRaisesRegex(ValueError, "amd64"):
            prepare.stage(self.root, self.inputs, self.root / "out")
        self.assertFalse((self.root / "out").exists())

    def test_mismatched_build_checksum_is_rejected(self):
        (self.inputs / "SHA256SUMS.txt").write_text("0" * 64 + "  " + self.exe.name + "\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "checksum"):
            prepare.stage(self.root, self.inputs, self.root / "out")

    def test_ambiguous_build_input_is_rejected(self):
        (self.inputs / "duplicate").mkdir()
        (self.inputs / "duplicate" / self.exe.name).write_bytes(self.exe.read_bytes())
        with self.assertRaisesRegex(ValueError, "exactly one"):
            prepare.stage(self.root, self.inputs, self.root / "out")

    def test_missing_notes_fail_even_in_build_only_mode(self):
        (self.root / "release/notes/1.2.3.md").unlink()
        with self.assertRaises(FileNotFoundError):
            prepare.metadata(self.root)

    def test_wrong_repository_is_rejected(self):
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "another/repo"}):
            with self.assertRaisesRegex(ValueError, "canonical"):
                prepare.metadata(self.root)

    def test_archive_traversal_is_rejected(self):
        archive = self.inputs / "companion.zip"
        with zipfile.ZipFile(archive, "w") as output:
            output.writestr("../escape.txt", "bad")
        with self.assertRaisesRegex(ValueError, "Unsafe"):
            prepare.verify_asset(archive, archive.name, {}, {})

    def test_debian_package_version_must_match(self):
        package = self.inputs / "test.deb"
        package.write_bytes(b"fixture")
        with patch.object(prepare.subprocess, "check_output", side_effect=["test\n", "0.1.0\n"]):
            with self.assertRaisesRegex(ValueError, "Version"):
                prepare.verify_asset(package, package.name, {"version": "1.2.3"}, {"package": "test"})


if __name__ == "__main__":
    unittest.main()
