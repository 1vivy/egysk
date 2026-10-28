# Build and contributor procedures

This document gives the implemented build paths and checks. Commands run from the repository root.

## Prerequisites

|Input|Required value|How to supply|
|---|---|---|
|Linux host|x86_64 host tools|The recipes and CI assume a Linux host with `linux-x86_64` NDK prebuilts.|
|Git submodules|`magisk/vendor/Magisk` at `e8915d9db15f5aae93973ffe65068e34df375a6a` (`v31.0-81-ge8915d9db`)|`git submodule update --init --recursive`|
|Python|3.x with the standard library|`python3` on `PATH`|
|`just`|Any current release|`just` on `PATH`|
|Rust|Stable `cargo` and `rustc` for the host workspace|`rustup`, or a distribution toolchain|
|Android NDK r29|`Pkg.Revision = 29.0.14206865`|`ANDROID_NDK_HOME`, or `--ndk-root` for the Cargo config generator|
|ONDK r30.1|`out/toolchains/ondk-r30.1` with `ONDK_VERSION` and `ndk-build`|`python3 tools/bootstrap_ondk.py`|
|BusyBox prebuilt|`out/cache/busybox/b4d0551f...40951/busybox-1.36.1.1.zip`|`magisk/scripts/package_direct.py` fetches it once and verifies its SHA-256|

The recipes write only below `out/`. `out/` is ignored. No recipe reads a sibling checkout,
`legacy/`, or `scratch/`.

## Bootstrap the pinned inputs

```sh
git submodule update --init --recursive
just bootstrap
```

`just bootstrap` runs the submodule update and then `python3 tools/bootstrap_ondk.py`. The
installer downloads the ONDK r30.1 archive for the host platform once, verifies the SHA-256 pin in
`tools/bootstrap_ondk.py`, and extracts it below `out/toolchains`. A second run reports the
installed tree and downloads nothing. It refuses an incomplete tree unless you pass `--force`.
Set `EGYSK_ONDK` or the first argument of `just build_native` to use another ONDK path.

## Native core

```sh
export ANDROID_NDK_HOME=/path/to/toolchains/android-ndk-r29
just build_native
```

The recipe runs `magisk/scripts/magisk.py ... build` with the ONDK path, the static NDK path from
`ANDROID_NDK_HOME`, and the `arm64-v8a` ABI. It writes `egyskinit`, `egyskd`, `magiskpolicy`,
`init-ld`, and `util_functions.sh` to `out/native/<abi>/bin`.

Build one ABI directly when you need another architecture:

```sh
python3 magisk/scripts/magisk.py --output out/Magisk build \
  --ndk out/toolchains/ondk-r30.1 --static-ndk "$ANDROID_NDK_HOME" --abi x86_64
```

The producer has these preconditions (`magisk/scripts/magisk.py`):

- `magisk/vendor/Magisk` is exactly at `PIN` with no tracked modifications.
- `--output` is a fresh or matching immediate child of `out/`. Changed sources require a new
  output path. The producer never overwrites a differing generated tree.
- The ONDK path contains `ndk-build`.
- The static NDK reports `Pkg.Revision = 29.`, and its API-35 sysroot contains `libc.a`.

Magiskinit links the full static Bionic from NDK r29. ONDK r30.1 cannot serve the static Bionic
input, because its `libc.a` embeds a Rust standard library and the link reports a duplicate
`rust_eh_personality` symbol. ONDK still supplies the compiler and Rust toolchain.

## Boot-payload LKM loader

```sh
export ANDROID_NDK_HOME=/path/to/toolchains/android-ndk-r29
just build_lkm_loader
```

The recipe builds the static `lkm-loader` for `aarch64-linux-android` at API 35 and installs it at
`out/lkm-loader/<triple>/lkm-loader`. It also sets a 16 KiB maximum page size. Pass another
architecture as the first argument.

## Host archive builder

```sh
just build_host
```

The result is `out/host/release/egysk-build-cpio`. The builder consumes explicit inputs and
downloads nothing:

```sh
out/host/release/egysk-build-cpio --build-cpio --legacy-lz4 \
  --kmi android16-6.12-6 --arch aarch64 --artifact-dir /path/to/releases \
  --bootstrap-dir /path/to/bootstrap --entry egyskinit \
  --out /path/to/egysk.cpio.lz4
```

`--legacy-lz4` selects the LZ4 legacy format with magic `0x184c2102`. The flag name is part of the
published command line, and maOS packaging passes it. The bootstrap directory supplies
`egysk.toml`, native companions, module objects, and independent early tools.

The builder admits no `*.compat.json` or `*.build.json` member. It never supplies a stock `/init`,
a second Magiskinit, or a kernel helper. A KMI selector must resolve to exactly one generation for
the architecture. The builder checks payload ELF architecture, static linkage for early
executables, module `ET_REL` type, module internal name, and vermagic flags
(`tools/egysk-tools/src/main.rs`).

`egysk-build.json` records architecture, exact KMI, entry, and member hashes, sizes, and modes. It
excludes itself and carries no package build ID. The platform packager owns the product `build-id`
and the schema-2 `set.json`. These files are admission data, not runtime receipts.

## Direct ESP package

```sh
export ANDROID_NDK_HOME=/path/to/toolchains/android-ndk-r29
just build_direct android16-6.12-6 out/direct-esp
```

The recipe builds the native core, the LKM loader, and the host builder, then runs
`magisk/scripts/package_direct.py`. Arguments are KMI, output tree, architecture (`aarch64` by
default), generated source tree (`out/Magisk-direct` by default), and ONDK path.

The output tree contains `egysk/build-id`, `egysk/egysk.toml`, `egysk/bin/{egyskd,magiskpolicy,
init-ld,util_functions.sh,busybox}`, `egysk/modules/lkm-loader/{module.prop,pid1.sh,bin/lkm-loader}`,
`egysk/modules_order`, and `direct/egysk.cpio`.

The packager takes BusyBox from Magisk's hash-pinned prebuilt archive, not from an operator input.
It refuses an existing output path. It writes no device and creates no launcher entry. Copy a
direct tree only to a dedicated ESP package, because managed and direct backing descriptors cannot
share one active `egysk/egysk.toml`.

## Cargo Android targets

Generate local Cargo settings before you build a Cargo target for Android:

```sh
python3 tools/setup_cargo_config.py --ndk-root /path/to/toolchains/android-ndk-r29
# or:
ANDROID_NDK_HOME=/path/to/toolchains/android-ndk-r29 python3 tools/setup_cargo_config.py --force
```

The generator requires `Pkg.Revision = 29.` and writes `.cargo/config.toml`, which is not tracked.
It configures API 35 wrappers for `aarch64-linux-android` and `x86_64-linux-android` and stops
when the revision, host tag, compiler wrappers, `llvm-ar`, or sysroot is absent. It writes only
inside the repository.

## Patch and pin procedure

`magisk/patches/Magisk/series` is the modification authority. The materializer applies the series
with no fuzz and no offset search, so every context byte must match the pinned revision. Product
logic belongs in owned sources under `magisk/product` and `magisk/runtime`. Patch hunks expose
integration points.

`magisk/product/identity.json` is the single source for native identity constants. The producer
generates `product/identity.rs` and `product/identity.hpp` from it.

To change the Magisk revision, update `PIN` in `magisk/scripts/magisk.py`, regenerate the patch
series against that revision, and run the materialization check.

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 magisk/scripts/magisk.py materialize --check
```

`materialize --check` compares a generated tree to the production series. Pass `--output` only
when the tree is an immediate child of `out/`.

The Android-target clippy path in CI needs an NDK and target setup:

```sh
export ANDROID_NDK_HOME=$PWD/out/toolchains/ondk-r30.1
rustup target add aarch64-linux-android x86_64-linux-android
source .github/scripts/setup-rust-build.sh aarch64-linux-android 26
source .github/scripts/setup-rust-build.sh x86_64-linux-android 26
CARGO_TARGET_DIR=out/clippy cargo clippy --target aarch64-linux-android --release
CARGO_TARGET_DIR=out/clippy cargo clippy --target x86_64-linux-android --release
```

|Workflow|Coverage|
|---|---|
|`.github/workflows/rustfmt.yml`|`cargo fmt --all --check` on a nightly toolchain.|
|`.github/workflows/clippy.yml`|On-demand ONDK bootstrap, then clippy for both Android targets.|
|`.github/workflows/daemon.yml`|NDK r29 download with SHA-256 check, ONDK bootstrap, exact materialization, both ABIs of the native core, and a materialization check.|
|`.github/workflows/daemon-extra.yml`|`cargo test --locked --workspace`, the release host build, and `egysk-build-cpio --help`.|
|`.github/workflows/build-core.yml`|Dispatch-only artifacts from a self-hosted runner with an operator-supplied NDK r29 path.|
|`.github/workflows/build.yml`|Calls the two ABI builds and the host build.|
|`.github/workflows/release.yml`|Release job on `v*` tags and manual dispatch.|
|`.github/workflows/shellcheck.yml`|ShellCheck on pushes to `main` and `dev`.|

Host tests and successful linking do not prove a boot. Build both Android architectures, exercise
the archive through a real consumer, and record the result separately from physical-device
qualification.

## Outputs

|Path|Content|
|---|---|
|`out/Magisk*`|Generated native source trees.|
|`out/toolchains/ondk-r30.1`|Bootstrapped ONDK.|
|`out/cache/busybox/<sha256>`|Hash-verified BusyBox archive.|
|`out/native/<abi>/bin`|Android binaries and installer helper.|
|`out/lkm-loader/<triple>`|Static LKM loader.|
|`out/host/release`|Host archive builder.|
|`out/direct-esp` or your path|Direct ESP tree.|
