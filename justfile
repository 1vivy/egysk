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
