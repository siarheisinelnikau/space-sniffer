import stat
import struct
import tempfile
import unittest
from pathlib import Path

from package_linux import architecture, stage


class LinuxPackageTests(unittest.TestCase):
    def test_elf_architecture_and_rejecting_foreign_or_truncated_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary) / "app"
            header = bytearray(64)
            header[:7] = b"\x7fELF\x02\x01\x01"
            for machine, expected in [(62, "amd64"), (183, "arm64")]:
                struct.pack_into("<HH", header, 16, 3, machine)
                binary.write_bytes(header)
                self.assertEqual(architecture(binary), expected)
            for data in [b"MZ", b"\x7fELF", bytes(64), header[:32]]:
                binary.write_bytes(data)
                with self.assertRaises(ValueError):
                    architecture(binary)
            struct.pack_into("<HH", header, 16, 1, 62)  # Object file, not executable.
            binary.write_bytes(header)
            with self.assertRaises(ValueError):
                architecture(binary)

    def test_staging_preserves_binary_permissions_metadata_and_existing_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            binary = work / "input"
            binary.write_bytes(b"unchanged application bytes")
            root = work / "package"
            stage(binary, root, "1.2.3", "amd64", "libc6 (>= 2.35), libgl1")
            installed = root / "usr/bin/space-sniffer"
            self.assertEqual(installed.read_bytes(), binary.read_bytes())
            self.assertEqual(stat.S_IMODE(installed.stat().st_mode), 0o755)
            self.assertEqual(stat.S_IMODE(root.stat().st_mode), 0o755)
            self.assertEqual(stat.S_IMODE((root / "usr/share/doc/space-sniffer/licenses/rust-auto-updator.txt").stat().st_mode), 0o644)
            control = (root / "DEBIAN/control").read_text()
            self.assertIn("Architecture: amd64\n", control)
            self.assertIn("Depends: libc6 (>= 2.35), libgl1\n", control)
            self.assertTrue((root / "usr/share/doc/space-sniffer/copyright").is_file())
            desktop = (root / "usr/share/applications/space-sniffer.desktop").read_text()
            self.assertIn("Exec=space-sniffer %f\n", desktop)
            self.assertFalse((root / "DEBIAN/postinst").exists())
            with self.assertRaises(FileExistsError):
                stage(binary, root, "9.9.9", "amd64", "libc6")
            self.assertEqual((root / "DEBIAN/control").read_text(), control)


if __name__ == "__main__":
    unittest.main()
