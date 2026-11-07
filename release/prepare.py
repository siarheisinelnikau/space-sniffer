"""Prepare release assets from this run's checked builds; never publish or launch apps."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tarfile
import tomllib
import zipfile


ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def simple_name(name):
    if not name or name in {".", ".."} or re.search(r"[\\/\x00-\x1f]", name):
        raise ValueError("Release assets must have simple filenames")
    return name


def configuration(root):
    return json.loads((root / "release/config.json").read_text(encoding="utf-8"))


def metadata(root):
    config = configuration(root)
    if config["kind"] == "go":
        source = (root / "internal/buildinfo/version.go").read_text(encoding="utf-8")
        version = re.search(r'^const Version = "([^"]+)"$', source, re.M).group(1)
    else:
        cargo = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
        version = (cargo["workspace"]["metadata"]["release"]["version"]
                   if config["kind"] == "source" else cargo["package"]["version"])
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version):
        raise ValueError("Only stable semantic versions can be published")
    notes_path = f"release/notes/{version}.md"
    notes = (root / notes_path).read_text(encoding="utf-8")
    if len(notes.strip()) < 100 or version not in notes:
        raise ValueError("Add reviewed English release notes for this exact version")
    repository = os.environ.get("GITHUB_REPOSITORY", config["repository"])
    if repository != config["repository"]:
        raise ValueError("Release repository does not match the canonical project")
    return {"repository": repository, "product": config["product"], "version": version,
            "tag": f"v{version}", "notes": notes_path,
            "notes_sha256": hashlib.sha256(notes.encode()).hexdigest(),
            "commit": os.environ.get("GITHUB_SHA", "")}


def verified_pe(path):
    with path.open("rb") as stream:
        header = stream.read(64)
        if len(header) != 64 or header[:2] != b"MZ":
            raise ValueError(f"Expected a Windows executable: {path.name}")
        offset = struct.unpack_from("<I", header, 60)[0]
        if offset < 64 or offset + 6 > path.stat().st_size:
            raise ValueError("Invalid PE header offset")
        stream.seek(offset)
        if stream.read(6) != b"PE\0\0\x64\x86":
            raise ValueError("Expected a Windows amd64 executable")


def verify_asset(path, name, meta, config):
    if not path.is_file() or path.is_symlink() or not path.stat().st_size:
        raise ValueError(f"Missing, linked or empty release asset: {name}")
    if name.endswith(".exe"):
        verified_pe(path)
    elif name.endswith(".deb"):
        for field, expected in {"Package": config["package"], "Version": meta["version"],
                                "Architecture": "amd64"}.items():
            actual = subprocess.check_output(["dpkg-deb", "-f", str(path), field], text=True).strip()
            if actual != expected:
                raise ValueError(f"Debian package {field} differs from the requested release")
    elif name.endswith(".zip"):
        with zipfile.ZipFile(path) as archive:
            if not archive.namelist() or archive.testzip():
                raise ValueError("The release archive is empty or corrupt")
            for item in archive.infolist():
                if item.filename.startswith(("/", "\\")) or ".." in Path(item.filename.replace("\\", "/")).parts:
                    raise ValueError("Unsafe archive entry")
    elif name.endswith(".tar.gz"):
        with tarfile.open(path) as archive:
            items = archive.getmembers()
            if not items:
                raise ValueError("Empty release archive")
            for item in items:
                if item.name.startswith("/") or ".." in Path(item.name).parts or item.issym() or item.islnk():
                    raise ValueError("Unsafe release archive entry")


def verify_supplied_checksum(path):
    candidates = [path.parent / (path.name + ".sha256"), path.parent / "SHA256SUMS.txt"]
    for candidate in candidates:
        if candidate.is_file():
            matches = []
            for line in candidate.read_text(encoding="utf-8-sig").splitlines():
                match = re.fullmatch(r"([a-fA-F0-9]{64})\s+\*?(.+)", line.strip())
                if match and match.group(2) == path.name:
                    matches.append(match.group(1).lower())
            # Some established packages checksum only their canonical executable.
            # Unlisted ancillary files still receive fresh checksums in the final candidate.
            if matches and matches != [digest(path)]:
                raise ValueError(f"Build checksum is missing, ambiguous or mismatched: {path.name}")


def notice_archive(root, input_root, destination):
    tracked = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
    license_pattern = re.compile(r"(^|/)(LICENSE[^/]*|LICENCE[^/]*|COPYING[^/]*|NOTICE[^/]*|THIRD[-_]?PARTY[^/]*|DEPENDENCY_LICENSES[^/]*)(/|$)|(^|/)licenses/", re.I)
    paths = [(root / name, "source/" + name) for name in tracked if name and license_pattern.search(name)]
    if not any(re.fullmatch(r"LICENSE(?:\.[^/]*)?", p.name, re.I) for p, _ in paths):
        raise ValueError("The root project license must be tracked")
    paths += [(p, "package/" + p.relative_to(input_root).as_posix())
              for p in input_root.rglob("*") if p.is_file() and license_pattern.search(p.name)]
    with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for source, name in sorted(paths, key=lambda pair: pair[1]):
            if source.is_symlink() or not source.is_file():
                raise ValueError("A license source is missing or linked")
            info = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, source.read_bytes())


def stage(root, input_root, output):
    meta = metadata(root)
    config = configuration(root)
    if output.exists():
        raise ValueError("Use a fresh staging directory")
    files = [p for p in input_root.rglob("*") if p.is_file()]
    selected = []
    for entry in config["assets"]:
        name = simple_name(entry["name"].format(version=meta["version"]))
        source_name = simple_name(entry.get("source", entry["name"]).format(version=meta["version"]))
        matches = [p for p in files if p.name == source_name]
        if entry.get("repository_file"):
            matches = [root / entry["repository_file"]]
        if len(matches) != 1:
            raise ValueError(f"Expected exactly one checked asset {source_name}, got {len(matches)}")
        source = matches[0]
        if source.is_symlink() or not source.resolve().is_relative_to(root.resolve() if entry.get("repository_file") else input_root.resolve()):
            raise ValueError("An asset escaped its input directory")
        verify_asset(source, name, meta, config)
        if not entry.get("repository_file"):
            verify_supplied_checksum(source)
        if name in [n for n, _ in selected]:
            raise ValueError("Duplicate release asset name")
        selected.append((name, source))
    output.mkdir(parents=True)
    for name, source in selected:
        shutil.copyfile(source, output / name)
    names = [name for name, _ in selected]
    if config.get("notices"):
        name = simple_name(f'{config["product"]}-v{meta["version"]}-notices.zip')
        notice_archive(root, input_root, output / name)
        names.append(name)
    checksums = "".join(f"{digest(output / name)}  {name}\n" for name in names)
    (output / "SHA256SUMS.txt").write_text(checksums, encoding="utf-8", newline="\n")
    names.append("SHA256SUMS.txt")
    for template in config.get("checksum_aliases", []):
        name = simple_name(template.format(version=meta["version"]))
        text = checksums.splitlines()[0] + "\n" if name.endswith(".sha256") else checksums
        (output / name).write_text(text, encoding="utf-8", newline="\n")
        names.append(name)
    meta["assets"] = [{"name": n, "size": (output / n).stat().st_size,
                       "digest": "sha256:" + digest(output / n)} for n in names]
    (output / "_manifest.json").write_text(json.dumps(meta, indent=2) + "\n", encoding="utf-8")
    print(f'Prepared {len(names)} verified assets for {meta["tag"]}')
    return meta


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["metadata", "stage"])
    args = parser.parse_args()
    if args.command == "metadata":
        meta = metadata(ROOT)
        for key in ["version", "tag"]:
            if os.environ.get("GITHUB_OUTPUT"):
                with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
                    output.write(f"{key}={meta[key]}\n")
        print(json.dumps(meta))
    else:
        stage(ROOT, ROOT / ".release-input", ROOT / ".release-stage")


if __name__ == "__main__":
    main()
