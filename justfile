# Patched Magisk initializer and daemon, with r29 static Bionic for PID1.
build_native ndk static_ndk abi="arm64-v8a":
    python3 scripts/magisk.py build --ndk "{{ndk}}" --static-ndk "{{static_ndk}}" --abi "{{abi}}"


# Linux host standalone archive builder.
build_host:
    cargo build --locked --release --package egysk-tools
clippy:
    cargo clippy --locked --workspace --all-targets -- -D warnings

format:
    cargo fmt --all
