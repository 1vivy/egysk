# Patched Magisk initializer and daemon, with r29 static Bionic for PID1.
build_native ndk static_ndk abi="arm64-v8a":
    python3 magisk/scripts/magisk.py build --ndk "{{ndk}}" --static-ndk "{{static_ndk}}" --abi "{{abi}}"


# Static bootstrap LKM loader for the PID 1 module payload (NDK r29, API 35).
build_lkm_loader ndk arch="aarch64":
    #!/usr/bin/env bash
    set -euo pipefail
    triple="{{arch}}-linux-android"
    llvm="{{ndk}}/toolchains/llvm/prebuilt/linux-x86_64/bin"
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
    cargo build --locked --release --package egysk-tools
clippy:
    cargo clippy --locked --workspace --all-targets -- -D warnings

format:
    cargo fmt --all

# Build native core + stock vendor loader and publish an egysk-only direct ESP tree.
# Use a fresh output directory; the publisher never replaces an existing tree.
build_direct ndk static_ndk busybox kmi output arch="aarch64" tree="out/Magisk-direct":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{arch}}" in
      aarch64) abi=arm64-v8a ;;
      x86_64) abi=x86_64 ;;
      *) echo "unsupported architecture: {{arch}}" >&2; exit 1 ;;
    esac
    python3 magisk/scripts/magisk.py --output "{{tree}}" build --ndk "{{ndk}}" --static-ndk "{{static_ndk}}" --abi "$abi"
    just build_lkm_loader "{{static_ndk}}" "{{arch}}"
    cargo build --locked --release --package egysk-tools --target-dir out/host
    python3 magisk/scripts/package_direct.py --native-bin "out/native/$abi/bin" \
      --loader "out/lkm-loader/{{arch}}-linux-android/lkm-loader" --busybox "{{busybox}}" \
      --builder out/host/release/egysk-build-cpio --kmi "{{kmi}}" --arch "{{arch}}" --out "{{output}}"
