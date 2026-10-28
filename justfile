# Pinned ONDK r30.1 cache. Override it with EGYSK_ONDK.
ondk := env("EGYSK_ONDK", "out/toolchains/ondk-r30.1")


# Prepare the external inputs. The command is idempotent.
# It initializes the pinned Magisk submodules, then verifies or installs ONDK.
bootstrap:
    git submodule update --init --recursive
    python3 tools/bootstrap_ondk.py


# Patched Magisk initializer and daemon, with r29 static Bionic for PID1.
# The generated source tree must be a fresh immediate child of out/.
build_native ndk=ondk abi="arm64-v8a" tree="out/Magisk":
    : "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to the Android NDK r29 root}"
    python3 magisk/scripts/magisk.py --output "{{tree}}" build --ndk "{{ndk}}" --static-ndk "$ANDROID_NDK_HOME" --abi "{{abi}}"


# Static bootstrap LKM loader for the PID 1 module payload (NDK r29, API 35).
build_lkm_loader arch="aarch64":
    #!/usr/bin/env bash
    set -euo pipefail
    ndk="${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to the Android NDK r29 root}"
    triple="{{arch}}-linux-android"
    llvm="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin"
    linker="$llvm/${triple}35-clang"
    test -x "$linker"
    upper=$(printf '%s' "$triple" | tr 'a-z-' 'A-Z_')
    export "CARGO_TARGET_${upper}_LINKER=$linker"
    export "CC_${triple//-/_}=$linker"
    export "AR_${triple//-/_}=$llvm/llvm-ar"
    RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static"
    RUSTFLAGS="$RUSTFLAGS -C link-arg=-Wl,-z,max-page-size=16384"
    RUSTFLAGS="$RUSTFLAGS -C link-arg=$("$linker" --print-resource-dir)/lib/linux/libclang_rt.builtins-{{arch}}-android.a"
    export RUSTFLAGS
    target_dir=out/lkm-loader-target
    cargo build --locked --release --target "$triple" --target-dir "$target_dir" --package lkm-loader
    install -D -m 0755 "$target_dir/$triple/release/lkm-loader" "out/lkm-loader/$triple/lkm-loader"


# Linux host standalone archive builder.
build_host:
    cargo build --locked --release --package egysk-tools --target-dir out/host
clippy:
    cargo clippy --locked --workspace --all-targets -- -D warnings

format:
    cargo fmt --all

# Build native core + stock vendor loader and publish an egysk-only direct ESP tree.
# Use a fresh output directory; the publisher never replaces an existing tree.
build_direct kmi output arch="aarch64" tree="out/Magisk-direct" ndk=ondk:
    #!/usr/bin/env bash
    set -euo pipefail
    static_ndk="${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to the Android NDK r29 root}"
    export ANDROID_NDK_HOME="$static_ndk"
    case "{{arch}}" in
      aarch64) abi=arm64-v8a ;;
      x86_64) abi=x86_64 ;;
      *) echo "unsupported architecture: {{arch}}" >&2; exit 1 ;;
    esac
    python3 magisk/scripts/magisk.py --output "{{tree}}" build --ndk "{{ndk}}" --static-ndk "$static_ndk" --abi "$abi"
    just build_lkm_loader "{{arch}}"
    cargo build --locked --release --package egysk-tools --target-dir out/host
    python3 magisk/scripts/package_direct.py --native-bin "out/native/$abi/bin" \
      --loader "out/lkm-loader/{{arch}}-linux-android/lkm-loader" \
      --builder out/host/release/egysk-build-cpio --kmi "{{kmi}}" --arch "{{arch}}" --out "{{output}}"
