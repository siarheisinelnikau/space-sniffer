#!/usr/bin/env python3
"""Check the real CLI and capture the Linux GUI under Xvfb with isolated settings.

Build with `cargo build --locked --features screenshots` first. Requires xvfb,
xauth and Mesa/OpenGL runtime libraries. This is an X11 smoke check, not a
substitute for testing native Wayland and desktop file dialogs.
"""
import argparse
import os
from pathlib import Path
import struct
import subprocess
import tempfile


def smoke(binary: Path, output: Path, native_wayland: bool = False) -> None:
    binary = binary.resolve(strict=True)
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="space-sniffer-example-") as temporary:
        fixture = Path(temporary)
        for name, size in [("Videos/holiday.mp4", 80 * 1024**2),
                           ("Photos/summer.jpg", 35 * 1024**2),
                           ("Backups/archive.zip", 50 * 1024**2),
                           ("Projects/main.rs", 10 * 1024**2)]:
            path = fixture / name
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("wb") as stream:
                stream.truncate(size)
        cli = subprocess.run([str(binary), "--scan", str(fixture)],
                             capture_output=True, text=True, check=True, timeout=30)
        if "175.0 MiB · 4 files · 5 folders · 0 skipped · 0 links" not in cli.stdout:
            raise RuntimeError(f"Unexpected scan totals: {cli.stdout}")
        (output / "scan.txt").write_text(cli.stdout)
        for view in ["empty", "volumes", "updates"]:
            screenshot = output / f"{view}.png"
            screenshot.unlink(missing_ok=True)
            environment = dict(os.environ)
            if native_wayland:
                if not environment.get("WAYLAND_DISPLAY"):
                    raise RuntimeError("Run --native-wayland inside a Wayland desktop session.")
                environment.pop("DISPLAY", None)
            else:
                environment.pop("WAYLAND_DISPLAY", None)
                environment["LIBGL_ALWAYS_SOFTWARE"] = "1"
            environment.pop("EFRAME_SCREENSHOT_TO", None)
            environment.update({
                "SPACE_SNIFFER_CAPTURE_TO": str(screenshot),
                "SPACE_SNIFFER_SCREENSHOT_VIEW": view,
                "SPACE_SNIFFER_SCREENSHOT_LANGUAGE": "en",
                "SPACE_SNIFFER_SCREENSHOT_THEME": "dark",
                "XDG_CONFIG_HOME": str(output / "config" / view),
                "XDG_DATA_HOME": str(output / "data" / view),
                "XDG_CACHE_HOME": str(output / "cache" / view),
            })
            args = [] if view == "empty" else [str(fixture)]
            command = [str(binary), *args]
            if not native_wayland:
                command = ["xvfb-run", "-a", "-s", "-screen 0 1280x800x24", *command]
            subprocess.run(command, env=environment, check=True, timeout=60)
            data = screenshot.read_bytes()
            if len(data) < 1000 or data[:8] != b"\x89PNG\r\n\x1a\n":
                raise RuntimeError(f"Missing or invalid GUI capture: {screenshot}")
            width, height = struct.unpack_from(">II", data, 16)
            if width < 800 or height < 520:
                raise RuntimeError(f"Unexpected GUI size: {width}x{height}")
    backend = "Wayland" if native_wayland else "X11"
    print(f"CLI and {backend} GUI smoke checks passed; captures in {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/space-sniffer"))
    parser.add_argument("--output", type=Path, default=Path("target/linux-smoke"))
    parser.add_argument("--native-wayland", action="store_true",
                        help="Use the current Wayland desktop instead of Xvfb")
    arguments = parser.parse_args()
    smoke(arguments.binary, arguments.output, arguments.native_wayland)
