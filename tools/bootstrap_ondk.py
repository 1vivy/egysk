#!/usr/bin/env python3
"""Install the pinned Magisk ONDK into the repository cache.

The installer downloads the archive from the ONDK release, verifies its SHA-256
digest, and extracts it below `out/toolchains`. A second run with a matching
`ONDK_VERSION` file does nothing. The installer never writes outside the
repository cache and never modifies the pinned Magisk submodule.
"""
from __future__ import annotations

import argparse
import hashlib
import platform
import shutil
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

VERSION = "r30.1"
ROOT = Path(__file__).resolve().parents[1]
DEFAULT_DEST = ROOT / "out" / "toolchains" / f"ondk-{VERSION}"
BASE_URL = f"https://github.com/topjohnwu/ondk/releases/download/{VERSION}"
SHA256 = {
    "linux": "a05e40a2b44e57c3103dd09721d3c2fccd3211e35db1ff9818e6f2a44e210289",
    "darwin": "a2aff59ac492b4a4019cbcf46db76311f27fd8b94ff7875952d68d22dd9ea13e",
    "windows": "5743de90f45704728a0d726d46671be23b377e7867f6acb106e1aee77463276a",
}


def os_name() -> str:
    system = platform.system().lower()
    names = {"linux": "linux", "darwin": "darwin", "windows": "windows"}
    try:
        return names[system]
    except KeyError:
        raise RuntimeError(f"unsupported host platform for ONDK: {platform.system()}") from None


def archive_name() -> str:
    return f"ondk-{VERSION}-{os_name()}.tar.xz"


def archive_url() -> str:
    return f"{BASE_URL}/{archive_name()}"


def file_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def installed(dest: Path) -> bool:
    version_file = dest / "ONDK_VERSION"
    if not version_file.is_file() or not (dest / "ndk-build").is_file():
        return False
    return version_file.read_text(encoding="utf-8").strip() == VERSION


def download(url: str, target: Path) -> None:
    print(f"downloading {url}")
    with urllib.request.urlopen(url) as response, target.open("wb") as stream:
        total = int(response.headers.get("Content-Length") or 0)
        copied = 0
        next_report = 64 << 20
        for block in iter(lambda: response.read(1 << 20), b""):
            stream.write(block)
            copied += len(block)
            if copied >= next_report:
                next_report += 64 << 20
                if total:
                    print(f"  {copied * 100 // total}% of {total >> 20} MiB")
                else:
                    print(f"  {copied >> 20} MiB")


def extract(archive: Path, work: Path, dest: Path) -> None:
    unpack = work / "unpack"
    unpack.mkdir()
    with tarfile.open(archive) as tar:
        if hasattr(tarfile, "data_filter"):
            tar.extractall(unpack, filter="tar")
        else:
            tar.extractall(unpack)
    tree = unpack / f"ondk-{VERSION}"
    if not tree.is_dir():
        candidates = [path for path in unpack.iterdir() if path.is_dir()]
        if len(candidates) != 1:
            raise ValueError(f"unexpected ONDK archive layout: {unpack}")
        tree = candidates[0]
    version_file = tree / "ONDK_VERSION"
    if not version_file.is_file():
        raise ValueError(f"ONDK_VERSION is absent from the archive: {tree}")
    if version_file.read_text(encoding="utf-8").strip() != VERSION:
        raise ValueError(f"archive does not contain ONDK {VERSION}: {version_file}")
    shutil.move(str(tree), str(dest))


def install(dest: Path, force: bool) -> None:
    if installed(dest):
        print(f"ONDK {VERSION} is present: {dest}")
        return
    if dest.exists():
        if not force:
            raise ValueError(f"incomplete ONDK tree exists: {dest}. Use --force to replace it.")
        shutil.rmtree(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    sha256 = SHA256[os_name()]
    with tempfile.TemporaryDirectory(prefix=".ondk-", dir=dest.parent) as temporary:
        work = Path(temporary)
        archive = work / archive_name()
        download(archive_url(), archive)
        actual = file_digest(archive)
        if actual != sha256:
            raise ValueError(f"SHA-256 mismatch for {archive.name}: {actual} != {sha256}")
        extract(archive, work, dest)
    if not installed(dest):
        raise ValueError(f"installed ONDK is incomplete: {dest}")
    print(f"ONDK {VERSION} is installed: {dest}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--dest",
        type=Path,
        default=DEFAULT_DEST,
        help=f"Install directory. Default: {DEFAULT_DEST.relative_to(ROOT)}",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Replace an incomplete or different tree at the destination.",
    )
    args = parser.parse_args()
    dest = args.dest.expanduser().absolute()
    try:
        install(dest, args.force)
    except (OSError, ValueError, RuntimeError) as error:
        parser.exit(1, f"bootstrap_ondk.py: {error}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
