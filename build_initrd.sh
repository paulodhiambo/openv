#!/bin/sh
# Rebuild all user-space binaries and repackage the initrd tar.
set -e

NIGHTLY_BIN="/Users/paul/.rustup/toolchains/nightly-aarch64-apple-darwin/bin"
export PATH="$NIGHTLY_BIN:$PATH"

REPO="/Users/paul/RustroverProjects/openv"
USER_DIR="$REPO/user"
TARGET="$USER_DIR/target/riscv64gc-unknown-none-elf/debug"
ROOT="$REPO/test_root"

echo "=== Building user-space workspace ==="
cargo build --manifest-path "$USER_DIR/Cargo.toml"

echo "=== Copying binaries to test_root ==="
for bin in init sh net-smoltcp ls cat hello producer consumer doexec; do
    if [ -f "$TARGET/$bin" ]; then
        cp "$TARGET/$bin" "$ROOT/$bin"
        echo "  copied $bin"
    fi
done

echo "=== Ensuring mount-point stub directories exist ==="
mkdir -p "$ROOT/proc" "$ROOT/dev"

echo "=== Rebuilding test_root.tar ==="
tar -cf "$REPO/test_root.tar" -C "$ROOT" .

echo "=== Done. test_root.tar updated ==="
ls -lh "$REPO/test_root.tar"
