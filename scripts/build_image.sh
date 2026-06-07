#!/bin/bash
# Build a bootable disk image (.img) for openv.
#
# Image layout (raw, no partition table):
#   Offset  1 MiB : kernel ELF  (openv)
#   Offset 32 MiB : initrd TAR  (test_root.tar)
#   Total         : 64 MiB
#
# The kernel is also extracted to a flat binary (openv.bin) suitable for
# direct loading by U-Boot or OpenSBI on real RISC-V hardware.
#
# Environment overrides (all optional):
#   KERNEL        path to kernel ELF   (default: debug build)
#   INITRD        path to initrd TAR   (default: test_root.tar)
#   OUTPUT        output image path    (default: openv.img)
#   LLVM_OBJCOPY  path to llvm-objcopy (auto-detected if unset)
#
# Boot with QEMU from the image:
#   qemu-system-riscv64 -machine virt -bios default -nographic \
#     -drive file=openv.img,format=raw,if=none,id=d0 \
#     -device virtio-blk-device,drive=d0
#
set -euo pipefail

cd "$(dirname "$0")/.."

KERNEL="${KERNEL:-target/riscv64gc-unknown-none-elf/debug/openv}"
INITRD="${INITRD:-test_root.tar}"
OUTPUT="${OUTPUT:-openv.img}"
IMG_SIZE_MB=64
KERNEL_OFFSET_MB=1
INITRD_OFFSET_MB=32

if [ ! -f "$KERNEL" ]; then
    echo "error: kernel not found: $KERNEL" >&2
    echo "       run 'make build-kernel' or 'cargo build --release' first" >&2
    exit 1
fi

if [ ! -f "$INITRD" ]; then
    echo "error: initrd not found: $INITRD" >&2
    echo "       run 'make initrd' first" >&2
    exit 1
fi

echo "Building disk image: $OUTPUT"
printf "  kernel : %s (%s)\n" "$KERNEL" "$(du -sh "$KERNEL" | cut -f1)"
printf "  initrd : %s (%s)\n" "$INITRD" "$(du -sh "$INITRD" | cut -f1)"
printf "  size   : %d MiB\n"  "$IMG_SIZE_MB"

# Create a zeroed image
dd if=/dev/zero of="$OUTPUT" bs=1M count="$IMG_SIZE_MB" status=none

# Write kernel ELF at 1 MiB
dd if="$KERNEL" of="$OUTPUT" bs=1M seek="$KERNEL_OFFSET_MB" conv=notrunc status=none
printf "  kernel written at offset %d MiB\n" "$KERNEL_OFFSET_MB"

# Write initrd at 32 MiB
dd if="$INITRD" of="$OUTPUT" bs=1M seek="$INITRD_OFFSET_MB" conv=notrunc status=none
printf "  initrd written at offset %d MiB\n" "$INITRD_OFFSET_MB"

# ── Flat binary (openv.bin) ───────────────────────────────────────────────────
# Resolve objcopy: prefer an explicit override, then system installs, then the
# llvm-objcopy bundled with the Rust nightly toolchain (llvm-tools-preview).
if [ -n "${LLVM_OBJCOPY:-}" ] && command -v "$LLVM_OBJCOPY" &>/dev/null; then
    OBJCOPY="$LLVM_OBJCOPY"
elif command -v riscv64-unknown-elf-objcopy &>/dev/null; then
    OBJCOPY="riscv64-unknown-elf-objcopy"
elif command -v llvm-objcopy &>/dev/null; then
    OBJCOPY="llvm-objcopy"
elif RUST_OBJCOPY=$(rustup which llvm-objcopy 2>/dev/null) && [ -n "$RUST_OBJCOPY" ]; then
    OBJCOPY="$RUST_OBJCOPY"
else
    OBJCOPY=""
fi

if [ -n "$OBJCOPY" ]; then
    "$OBJCOPY" -O binary "$KERNEL" openv.bin
    printf "  flat binary: openv.bin (%s)\n" "$(du -sh openv.bin | cut -f1)"
else
    echo "  (skipping flat binary — no objcopy found; install llvm-tools-preview or riscv64-unknown-elf-binutils)"
fi

echo ""
printf "Done: %s (%s)\n" "$OUTPUT" "$(du -sh "$OUTPUT" | cut -f1)"
echo ""
echo "Boot with QEMU (full image):"
echo "  qemu-system-riscv64 -machine virt -bios default -nographic \\"
echo "    -drive file=$OUTPUT,format=raw,if=none,id=d0 \\"
echo "    -device virtio-blk-device,drive=d0"
echo ""
echo "Boot with QEMU (direct kernel — simpler for development):"
echo "  ./scripts/run.sh"
