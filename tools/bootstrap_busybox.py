#!/usr/bin/env python3
"""Acquire Magisk's hash-pinned BusyBox prebuilt into the repository cache.

Pins and ABI member names match Magisk e8915d9, Setup.kt:104-107,141-142.
The archive is verified on every use, including immediately after download.
"""
from __future__ import annotations

import argparse
import shutil
import sys
import tempfile
import zipfile
from pathlib import Path

from bootstrap_ondk import ROOT, download, file_digest

VERSION = "1.36.1.1"
URL = f"https://github.com/topjohnwu/magisk-files/releases/download/files/busybox-{VERSION}.zip"
SHA256 = "b4d0551feabaf314e53c79316c980e8f66432e9fb91a69dbbf10a93564b40951"
ABIS = ("arm64-v8a", "armeabi-v7a", "x86_64", "x86", "riscv64")
CACHE = ROOT / "out/cache/busybox" / SHA256


def install(abi: str, output: Path) -> None:
    CACHE.mkdir(parents=True, exist_ok=True)
    archive = CACHE / f"busybox-{VERSION}.zip"
    if not archive.exists():
        with tempfile.TemporaryDirectory(prefix=".download-", dir=CACHE) as temporary:
            target = Path(temporary) / archive.name
            download(URL, target)
            actual = file_digest(target)
            if actual != SHA256:
                raise ValueError(f"SHA-256 mismatch for {URL}: {actual} != {SHA256}")
            target.rename(archive)
    actual = file_digest(archive)
    if actual != SHA256:
        raise ValueError(f"SHA-256 mismatch for cached archive {archive}: {actual} != {SHA256}")
    if output.is_symlink() or (output.exists() and not output.is_file()):
        raise ValueError(f"BusyBox output must be a regular file: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive) as contents:
        with contents.open(f"{abi}/libbusybox.so") as source, output.open("wb") as target:
            shutil.copyfileobj(source, target)
    output.chmod(0o755)
    print(f"BusyBox {VERSION} ({abi}): {output}; SHA-256 {file_digest(output)}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--abi", choices=ABIS, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    try:
        install(args.abi, args.out)
    except (OSError, ValueError, KeyError, zipfile.BadZipFile) as error:
        parser.exit(1, f"bootstrap_busybox.py: {error}\n")


if __name__ == "__main__":
    main()
