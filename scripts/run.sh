#!/bin/bash
set -e

cd "$(dirname "$0")/.."

KERNEL="target/riscv64gc-unknown-none-elf/debug/openv"
INITRD="test_root.tar"
MEMORY="${QEMU_MEM:-128M}"
CPUS="${QEMU_CPUS:-1}"

if [ ! -f "$KERNEL" ]; then
    echo "error: kernel not found at $KERNEL — run ./build.sh first"
    exit 1
fi

if [ ! -f "$INITRD" ]; then
    echo "error: initrd not found at $INITRD — run ./build.sh first"
    exit 1
fi

echo "Booting openv..."
echo "  kernel : $KERNEL ($(du -sh $KERNEL | cut -f1))"
echo "  initrd : $INITRD ($(du -sh $INITRD | cut -f1))"
echo "  memory : $MEMORY"
echo "  (Ctrl-A X to quit QEMU)"
echo ""

exec qemu-system-riscv64 \
    -machine virt \
    -bios default \
    -nographic \
    -m "$MEMORY" \
    -smp "$CPUS" \
    -initrd "$INITRD" \
    -kernel "$KERNEL"
