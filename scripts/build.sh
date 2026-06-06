#!/bin/bash
# Build kernel, userspace, and initrd in one step.
#
# Usage:
#   ./scripts/build.sh            # debug
#   ./scripts/build.sh --release  # release
#
# Override the binary list:
#   BINS="init sh ls" ./scripts/build.sh
set -e

cd "$(dirname "$0")/.."

BINS="${BINS:-init sh ls cat hello producer consumer doexec forktest net-smoltcp spin vfs-server echo-server pm-server rs-server virtio-blk-driver}"
USER_BUILD_DIR="debug"
KERNEL_FLAGS=""
USER_FLAGS=""

if [ "$1" = "--release" ]; then
    KERNEL_FLAGS="--release"
    USER_FLAGS="--release"
    USER_BUILD_DIR="release"
fi

# ── 1. Userspace ───────────────────────────────────────────────────────────────
echo "[1/3] Building userspace..."
(cd user && cargo build $USER_FLAGS)

# ── 2. Initrd ──────────────────────────────────────────────────────────────────
echo "[2/3] Packaging initrd..."
mkdir -p test_root/proc test_root/dev

for bin in $BINS; do
    src="user/target/riscv64gc-unknown-none-elf/$USER_BUILD_DIR/$bin"
    if [ -f "$src" ]; then
        cp "$src" "test_root/$bin"
        echo "  copied $bin"
    else
        echo "  (skipping $bin — not found at $src)"
    fi
done

echo "Hello from initrd TAR!" > test_root/dummy.txt

cd test_root
# Build tar from all entries, respecting subdirectories (proc/, dev/)
tar -cf ../test_root.tar .
cd ..
echo "  initrd: test_root.tar ($(du -sh test_root.tar | cut -f1))"

# ── 3. Kernel ──────────────────────────────────────────────────────────────────
echo "[3/3] Building kernel..."
cargo build $KERNEL_FLAGS

echo ""
echo "Build complete. Run with: ./scripts/run.sh"
