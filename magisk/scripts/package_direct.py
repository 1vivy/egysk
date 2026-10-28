#!/usr/bin/env python3
"""Assemble an egysk-only ESP tree from explicit native producer outputs.

BusyBox is not an input: it comes from Magisk's hash-pinned prebuilt archive
through tools/bootstrap_busybox.py, for the tree's architecture.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
from bootstrap_busybox import install as install_busybox  # noqa: E402

TOOLS = ("egyskd", "magiskpolicy", "init-ld", "util_functions.sh")
ABI = {"aarch64": "arm64-v8a", "x86_64": "x86_64"}


def copy(source: Path, target: Path, mode: int = 0o755) -> None:
    if source.is_symlink() or not source.is_file():
        raise ValueError(f"input must be a regular file: {source}")
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, target)
    target.chmod(mode)


def package(args: argparse.Namespace) -> None:
    if not re.fullmatch(r"android[0-9]+-[0-9]+\.[0-9]+-[0-9]+", args.kmi):
        raise ValueError("--kmi requires one exact branch-generation")
    output = args.out.absolute()
    if output.exists() or output.is_symlink():
        raise ValueError(f"refusing to replace existing output: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".egysk-direct-", dir=output.parent) as temporary:
        work = Path(temporary)
        tree = work / "esp"
        package_root = tree / "egysk"
        package_root.mkdir(parents=True)
        config = (f"kmi = {json.dumps(args.kmi)}\n"
                  '[backing]\nkind = "filesystem"\nsource = "/metadata"\n'
                  'fs_type = "f2fs"\nmount_at = "/metadata"\n')
        (package_root / "egysk.toml").write_text(config)
        for name in TOOLS:
            copy(args.native_bin / name, package_root / "bin" / name)
        busybox = work / "busybox"
        install_busybox(ABI[args.arch], busybox)
        copy(busybox, package_root / "bin/busybox")
        module = package_root / "modules/lkm-loader"
        for name in ("module.prop", "pid1.sh"):
            copy(ROOT / "modules/lkm-loader" / name, module / name,
                 0o755 if name.endswith(".sh") else 0o644)
        copy(args.loader, module / "bin/lkm-loader")
        (package_root / "modules_order").write_text("lkm-loader\n")
        bootstrap = work / "bootstrap"
        bootstrap.mkdir()
        (bootstrap / "egysk.toml").write_text(config)
        shutil.copytree(package_root / "bin", bootstrap / "egysk/bin")
        shutil.copytree(package_root / "modules", bootstrap / "egysk/modules")
        artifacts = work / "artifacts"
        copy(args.native_bin / "egyskinit", artifacts / args.kmi / args.arch / "egyskinit")
        (tree / "direct").mkdir()
        subprocess.run([str(args.builder), "--build-cpio", "--legacy-lz4",
                        "--kmi", args.kmi, "--arch", args.arch,
                        "--artifact-dir", str(artifacts), "--bootstrap-dir", str(bootstrap),
                        "--entry", "egyskinit", "--out", str(tree / "direct/egysk.cpio")],
                       check=True)
        identity = hashlib.sha256()
        for path in sorted(p for p in tree.rglob("*") if p.is_file()):
            identity.update(str(path.relative_to(tree)).encode() + b"\0")
            identity.update(path.stat().st_mode.to_bytes(4, "little"))
            identity.update(hashlib.sha256(path.read_bytes()).digest())
        (package_root / "build-id").write_text(identity.hexdigest()[:12] + "\n")
        tree.rename(output)
    print(f"Direct ESP tree: {output} (no BLS entry or device writes)")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--native-bin", type=Path, required=True)
    parser.add_argument("--loader", type=Path, required=True)
    parser.add_argument("--builder", type=Path, required=True)
    parser.add_argument("--kmi", required=True)
    parser.add_argument("--arch", choices=("aarch64", "x86_64"), required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    try:
        package(args)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"package_direct.py: {error}\n")


if __name__ == "__main__":
    main()
