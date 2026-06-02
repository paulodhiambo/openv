#!/bin/bash
# Build a bootable disk image (.img) for openv.
#
# The image layout:
#   Sector 0        : MBR / raw padding
#   Offset 1 MiB    : kernel ELF (openv)
#   Offset 32 MiB   : initrd (test_root.tar)
#
# Boot with QEMU:
#   qemu-system-riscv64 -machine virt -bios default -nographic \
#     -drive file=openv.img,format=raw,if=virtio
#
# Or keep using -kernel/-initrd flags (simpler for development):
#   ./scripts/run.sh
#
set -e

cd "$(dirname "$0")/.."

KERNEL="target/riscv64gc-unknown-none-elf/debug/openv"
INITRD="test_root.tar"
OUTPUT="openv.img"
IMG_SIZE_MB=64        # total image size
KERNEL_OFFSET_MB=1    # kernel placed at 1 MiB
INITRD_OFFSET_MB=32   # initrd placed at 32 MiB

if [ ! -f "$KERNEL" ]; then
    echo "error: kernel not found — run ./scripts/build.sh first"
    exit 1
fi

if [ ! -f "$INITRD" ]; then
    echo "error: initrd not found — run ./scripts/build.sh first"
    exit 1
fi

echo "Building disk image: $OUTPUT"
echo "  kernel : $KERNEL"
echo "  initrd : $INITRD"
echo "  size   : ${IMG_SIZE_MB} MiB"

# Create a zeroed image
dd if=/dev/zero of="$OUTPUT" bs=1M count="$IMG_SIZE_MB" status=none

# Write kernel at 1 MiB offset
dd if="$KERNEL" of="$OUTPUT" bs=1M seek="$KERNEL_OFFSET_MB" conv=notrunc status=none
echo "  kernel written at offset ${KERNEL_OFFSET_MB} MiB"

# Write initrd at 32 MiB offset
dd if="$INITRD" of="$OUTPUT" bs=1M seek="$INITRD_OFFSET_MB" conv=notrunc status=none
echo "  initrd written at offset ${INITRD_OFFSET_MB} MiB"

# Also produce a stripped flat binary of just the kernel
KERNEL_BIN="${KERNEL%.elf}.bin"
if command -v riscv64-unknown-elf-objcopy &>/dev/null; then
    riscv64-unknown-elf-objcopy -O binary "$KERNEL" openv.bin
    echo "  flat binary: openv.bin ($(du -sh openv.bin | cut -f1))"
elif command -v llvm-objcopy &>/dev/null; then
    llvm-objcopy -O binary "$KERNEL" openv.bin
    echo "  flat binary: openv.bin ($(du -sh openv.bin | cut -f1))"
else
    echo "  (skipping flat binary — objcopy not found)"
fi

echo ""
echo "Done: $OUTPUT ($(du -sh $OUTPUT | cut -f1))"
echo ""
echo "Run with QEMU:"
echo "  qemu-system-riscv64 -machine virt -bios default -nographic \\"
echo "    -device loader,file=$KERNEL,addr=0x80200000 \\"
echo "    -initrd $INITRD"
echo ""
echo "Or just use: ./scripts/run.sh  (recommended for development)"
