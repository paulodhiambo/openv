#!/bin/bash
set -e

# Always run from the project root regardless of where the script is invoked from
cd "$(dirname "$0")/.."

TOOLCHAIN_BIN="$HOME/.rustup/toolchains/nightly-aarch64-apple-darwin/bin"
export PATH="$TOOLCHAIN_BIN:$PATH"

# ── Userspace ─────────────────────────────────────────────────────────────────
echo "[1/3] Building userspace..."
(cd user && cargo build)

# ── Initrd ────────────────────────────────────────────────────────────────────
echo "[2/3] Packaging initrd..."
mkdir -p test_root

copy_if_exists() {
    local src="user/target/riscv64gc-unknown-none-elf/debug/$1"
    local dst="test_root/$2"
    if [ -f "$src" ]; then
        cp "$src" "$dst"
        echo "  copied $1 → $dst"
    else
        echo "  warning: $src not found, skipping"
    fi
}

copy_if_exists init    init
copy_if_exists sh      sh
copy_if_exists ls      ls
copy_if_exists cat     cat
copy_if_exists producer producer
copy_if_exists consumer consumer
copy_if_exists net-smoltcp net-smoltcp

echo "Hello from initrd TAR!" > test_root/dummy.txt

# Build tar from whatever is present in test_root
(
    cd test_root
    FILES=$(find . -maxdepth 1 -not -name '.' | sed 's|^\./||' | tr '\n' ' ')
    tar -cf ../test_root.tar $FILES
    echo "  initrd: test_root.tar ($(du -sh ../test_root.tar | cut -f1))"
)

# ── Kernel ────────────────────────────────────────────────────────────────────
echo "[3/3] Building kernel..."
cargo build

echo ""
echo "Build complete. Run with: ./scripts/run.sh"
