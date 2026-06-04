#!/bin/bash
set -e

cd "$(dirname "$0")/.."

RELEASE=""
if [ "$1" = "--release" ]; then
    RELEASE="release"
else
    RELEASE="debug"
fi

KERNEL="target/riscv64gc-unknown-none-elf/$RELEASE/openv"
INITRD="test_root.tar"
MEMORY="${QEMU_MEM:-128M}"
CPUS="${QEMU_CPUS:-1}"
DISK_IMG="disk.img"

if [ ! -f "$KERNEL" ]; then
    echo "error: kernel not found at $KERNEL — run ./scripts/build.sh first"
    exit 1
fi

if [ ! -f "$INITRD" ]; then
    echo "error: initrd not found at $INITRD — run ./scripts/build.sh first"
    exit 1
fi

echo "Booting openv..."
echo "  kernel : $KERNEL ($(du -sh "$KERNEL" | cut -f1))"
echo "  initrd : $INITRD ($(du -sh "$INITRD" | cut -f1))"
echo "  memory : $MEMORY"
echo "  cpus   : $CPUS"
echo "  (Ctrl-A X to quit QEMU)"
echo ""

QEMU_NET="-netdev user,id=net0 -device virtio-net-device,netdev=net0"
QEMU_DISK=""
if [ -f "$DISK_IMG" ]; then
    echo "  disk   : $DISK_IMG (persistent OFS)"
    QEMU_DISK="-drive id=disk0,file=$DISK_IMG,format=raw,if=none -device virtio-blk-device,drive=disk0"
fi

exec qemu-system-riscv64 \
    -machine virt \
    -bios default \
    -nographic \
    -m "$MEMORY" \
    -smp "$CPUS" \
    $QEMU_NET \
    -initrd "$INITRD" \
    -kernel "$KERNEL" \
    $QEMU_DISK
