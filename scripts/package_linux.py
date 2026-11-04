#!/usr/bin/env python3
"""Build an Ubuntu/Debian .deb from the shared release executable, without root.

Requires Python 3.11+ and dpkg-dev. Build on the oldest Ubuntu release you
intend to support: dpkg-shlibdeps records the actual linked ABI requirements.
Does not install the package, publish it, or replace an existing output file.
"""
import argparse
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import tomllib

REPOSITORY = Path(__file__).resolve().parents[1]
ARCHITECTURES = {62: "amd64", 183: "arm64"}
# These backends are loaded at runtime, so shlibdeps cannot discover them.
DESKTOP_DEPENDENCIES = (
    "libgl1", "libegl1", "libx11-6", "libxcb1", "libxkbcommon0",
    "libxkbcommon-x11-0", "libwayland-client0", "libwayland-cursor0", "xdg-utils",
    "apt", "pkexec | policykit-1",
)


def architecture(binary: Path) -> str:
    with binary.open("rb") as source:
        header = source.read(64)
    if len(header) != 64 or header[:7] != b"\x7fELF\x02\x01\x01":
        raise ValueError("Expected a little-endian 64-bit ELF executable.")
    kind, machine = struct.unpack_from("<HH", header, 16)
    if kind not in (2, 3) or machine not in ARCHITECTURES:
        raise ValueError("Expected an amd64 or arm64 ELF executable.")
    return ARCHITECTURES[machine]


def stage(binary: Path, root: Path, version: str, arch: str, dependencies: str) -> None:
    root.mkdir()
    executable = root / "usr/bin/space-sniffer"
    executable.parent.mkdir(parents=True)
    shutil.copyfile(binary, executable)
    executable.chmod(0o755)
    for source, destination in (
        ("packaging/linux/space-sniffer.desktop", "usr/share/applications/space-sniffer.desktop"),
        ("packaging/linux/space-sniffer.svg", "usr/share/icons/hicolor/scalable/apps/space-sniffer.svg"),
        ("LICENSE", "usr/share/doc/space-sniffer/copyright"),
    ):
        target = root / destination
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(REPOSITORY / source, target)
        target.chmod(0o644)
    shutil.copytree(REPOSITORY / "licenses", root / "usr/share/doc/space-sniffer/licenses")
    control = root / "DEBIAN/control"
    control.parent.mkdir()
    control.write_text(
        f"Package: space-sniffer\nVersion: {version}\nArchitecture: {arch}\n"
        "Section: utils\nPriority: optional\n"
        "Maintainer: Space Sniffer contributors <siarheisinelnikau@users.noreply.github.com>\n"
        "Homepage: https://github.com/siarheisinelnikau/space-sniffer\n"
        f"Depends: {dependencies}\n"
        "Recommends: xdg-desktop-portal, xdg-desktop-portal-gtk | xdg-desktop-portal-kde\n"
        "Description: Live disk usage explorer\n"
        " Explore folders and mounted filesystems with an interactive treemap.\n",
        encoding="utf-8",
    )
    control.chmod(0o644)
    # Do not inherit a developer's group-writable umask in a system package.
    root.chmod(0o755)
    for path in root.rglob("*"):
        path.chmod(0o755 if path.is_dir() else 0o644)
    executable.chmod(0o755)


def package(binary: Path, output: Path) -> Path:
    binary = binary.resolve(strict=True)
    arch = architecture(binary)
    with (REPOSITORY / "Cargo.toml").open("rb") as source:
        version = tomllib.load(source)["package"]["version"]
    output.mkdir(parents=True, exist_ok=True)
    destination = output / f"space-sniffer_{version}_{arch}.deb"
    if destination.exists():
        raise FileExistsError(destination)
    with tempfile.TemporaryDirectory(prefix="space-sniffer-deb-") as temporary:
        work = Path(temporary)
        (work / "debian").mkdir()
        (work / "debian/control").write_text("Source: space-sniffer\n\nPackage: space-sniffer\nArchitecture: any\n")
        result = subprocess.run(
            ["dpkg-shlibdeps", "-O", f"-e{binary}"], cwd=work,
            check=True, capture_output=True, text=True,
        )
        linked = next(line.removeprefix("shlibs:Depends=") for line in result.stdout.splitlines()
                      if line.startswith("shlibs:Depends="))
        dependencies = ", ".join([linked, *DESKTOP_DEPENDENCIES])
        root = work / "package"
        stage(binary, root, version, arch, dependencies)
        archive = work / "package.deb"
        subprocess.run(["dpkg-deb", "--root-owner-group", "--build", str(root), str(archive)], check=True)
        # Exclusive creation protects a package produced by another concurrent run.
        with destination.open("xb") as target, archive.open("rb") as source:
            shutil.copyfileobj(source, target)
    return destination


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=REPOSITORY / "target/release/space-sniffer")
    parser.add_argument("--output", type=Path, default=REPOSITORY / "target/debian")
    args = parser.parse_args()
    try:
        print(package(args.binary, args.output))
    except (OSError, ValueError, StopIteration, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Could not package Linux app: {error}\n{getattr(error, 'stderr', '') or ''}")


if __name__ == "__main__":
    main()
