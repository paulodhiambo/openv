TARGET           := riscv64gc-unknown-none-elf
NIGHTLY          := $(shell rustup which cargo --toolchain nightly | sed 's|/cargo$$||')
CARGO            ?= cargo
RUSTC            ?= rustc
export PATH      := $(NIGHTLY):$(PATH)
export RUSTUP_TOOLCHAIN := nightly-aarch64-apple-darwin
KERNEL_DIR  := target/$(TARGET)/debug
KERNEL      := $(KERNEL_DIR)/openv
KERNEL_REL  := target/$(TARGET)/release/openv
INITRD      := test_root.tar
IMG         := openv.img
DISK_IMG    := disk.img
DISK_SIZE_MB := 8

# Overridable via env or make args
BINS       ?= init sh ls cat hello producer consumer doexec forktest net-smoltcp spin vfs-server pm-server rs-server echo-server net-client ipc_test virtio-blk-driver pkg epolltest procfs-server devfs-server component-manager
QEMU_MEM   ?= 512M
QEMU_CPUS  ?= 4
QEMU_FLAGS  = -machine virt -bios default -nographic -m $(QEMU_MEM) -smp $(QEMU_CPUS)
QEMU_NET    = -netdev user,id=net0 -device virtio-net-device,netdev=net0
QEMU_DISK   = -drive id=disk0,file=$(DISK_IMG),format=raw,if=none -device virtio-blk-device,drive=disk0

.PHONY: help build build-kernel build-user build-release build-relibc initrd \
        run all debug image image-release disk                                \
        clean clean-all clean-user check clippy fmt                           \
        $(BINS:%=test_root/%)

help:
	@echo 'openv — RISC-V 64-bit microkernel'
	@echo ''
	@echo 'Usage:  make <target> [BINS="init sh ..."] [QEMU_MEM=512M] [QEMU_CPUS=4]'
	@echo ''
	@echo 'Build'
	@echo '  all                Build kernel, userspace, initrd, then run (default)'
	@echo '  build              Build kernel, userspace, and package initrd'
	@echo '  build-release      Release build of everything'
	@echo '  build-kernel       Build kernel only (skips userspace and initrd)'
	@echo '  build-user         Build userspace binaries only'
	@echo '  initrd             Package initrd from already-built userspace bins'
	@echo ''
	@echo 'Run'
	@echo '  run                Boot in QEMU (creates disk.img if absent)'
	@echo '  all                Build + run'
	@echo '  debug              Boot with GDB server on :1234 (-s -S)'
	@echo ''
	@echo 'Release'
	@echo '  build-release      $(KERNEL_REL) + release userspace + initrd'
	@echo '  image              Build debug disk image (openv.img)'
	@echo '  image-release      Build release disk image (openv.img)'
	@echo ''
	@echo 'Quality'
	@echo '  check              cargo check (kernel + userspace)'
	@echo '  clippy             cargo clippy (kernel + userspace, CI-equivalent flags)'
	@echo '  fmt                cargo fmt (kernel + userspace)'
	@echo ''
	@echo 'Clean'
	@echo '  clean              Remove build artifacts (preserves disk.img OFS data)'
	@echo '  clean-all          Remove everything including disk.img'
	@echo '  clean-user         Remove userspace build artifacts only'
	@echo ''
	@echo 'Variables'
	@echo '  BINS       Binaries to include in initrd (default: $(BINS))'
	@echo '  QEMU_MEM   QEMU memory (default: $(QEMU_MEM))'
	@echo '  QEMU_CPUS  QEMU CPU count (default: $(QEMU_CPUS))'

# ── Default ────────────────────────────────────────────────────────────────────

all: build run

# ── Build ──────────────────────────────────────────────────────────────────────

build: build-user initrd build-kernel
	@echo 'Build complete.  Run: make run'

build-release: build-user-release initrd build-kernel-release
	@echo 'Release build complete.  Run: make run'

build-kernel:
	$(CARGO) build

build-kernel-release:
	$(CARGO) build --release

build-user:
	cd user && $(CARGO) build

build-user-release:
	cd user && $(CARGO) build --release

build-relibc:
	cd user/relibc && $(CARGO) build --release --target $(TARGET)
	@echo "relibc static library: user/relibc/target/$(TARGET)/release/librelibc.a"

initrd: test_root/proc test_root/dev
	@for bin in $(BINS); do \
	  src="user/target/$(TARGET)/debug/$$bin"; \
	  if [ -f "$$src" ]; then \
	    cp "$$src" "test_root/$$bin"; \
	    echo "  initrd: copied $$bin"; \
	  else \
	    echo "  initrd: (skipping $$bin)"; \
	  fi; \
	done
	@echo "Hello from initrd TAR!" > test_root/dummy.txt
	@cd test_root && tar -cf ../$(INITRD) .
	@echo '  initrd: $(INITRD) (size: $(shell du -sh $(INITRD) | cut -f1))'

test_root/proc test_root/dev:
	mkdir -p $@

# ── Run ────────────────────────────────────────────────────────────────────────

disk: $(DISK_IMG)

$(DISK_IMG):
	dd if=/dev/zero of=$(DISK_IMG) bs=1M count=$(DISK_SIZE_MB) 2>/dev/null
	@echo '  disk   : $(DISK_IMG) ($(DISK_SIZE_MB) MB)'

run: $(KERNEL) $(INITRD) $(DISK_IMG)
	@pkill -f 'qemu-system-riscv64' 2>/dev/null || true
	@echo 'Booting openv...'
	@echo '  kernel : $(KERNEL)'
	@echo '  initrd : $(INITRD)'
	@echo '  disk   : $(DISK_IMG) (persistent OFS)'
	@echo '  memory : $(QEMU_MEM)'
	@echo '  (Ctrl-A X to quit QEMU)'
	@echo ''
	qemu-system-riscv64 $(QEMU_FLAGS) $(QEMU_NET) -initrd $(INITRD) -kernel $(KERNEL) $(QEMU_DISK)

debug: $(KERNEL) $(INITRD) $(DISK_IMG)
	@pkill -f 'qemu-system-riscv64' 2>/dev/null || true
	@echo 'Booting openv with GDB server on :1234...'
	qemu-system-riscv64 $(QEMU_FLAGS) $(QEMU_NET) -initrd $(INITRD) -kernel $(KERNEL) $(QEMU_DISK) -s -S

# ── Disk image ─────────────────────────────────────────────────────────────────

image: $(KERNEL) $(INITRD)
	./scripts/build_image.sh

image-release: $(KERNEL_REL) $(INITRD)
	KERNEL=$(KERNEL_REL) ./scripts/build_image.sh

sd_write:
	sudo dd if=openv.img of=/dev/sdX bs=4M status=progress

# ── Quality ────────────────────────────────────────────────────────────────────

CLIPPY_ALLOW := \
  -A clippy::identity_op \
  -A clippy::collapsible_if \
  -A clippy::unnecessary_cast \
  -A clippy::extra_unused_lifetimes \
  -A clippy::implicit_saturating_sub \
  -A clippy::manual_contains \
  -A clippy::manual_div_ceil \
  -A clippy::manual_is_multiple_of \
  -A clippy::manual_range_contains \
  -A clippy::needless_range_loop \
  -A clippy::redundant_pattern_matching \
  -A clippy::slow_vector_initialization \
  -A clippy::unnecessary_map_or \
  -A clippy::while_let_loop \
  -A clippy::clone_on_copy \
  -A clippy::never_loop \
  -A clippy::new_without_default \
  -A clippy::unwrap_or_default \
  -A clippy::empty_line_after_doc_comments

check:
	$(CARGO) check
	cd user && $(CARGO) check

clippy:
	$(CARGO) clippy -- -D warnings $(CLIPPY_ALLOW)
	cd user && $(CARGO) clippy -- -D warnings $(CLIPPY_ALLOW)

fmt:
	$(CARGO) fmt
	cd user && $(CARGO) fmt

# ── Clean ──────────────────────────────────────────────────────────────────────

clean:
	rm -rf openv.img openv.bin
	cd user && $(CARGO) clean
	$(CARGO) clean

clean-all: clean
	rm -f $(DISK_IMG)

clean-user:
	cd user && $(CARGO) clean

# ── File dependencies ──────────────────────────────────────────────────────────

$(KERNEL):
	$(CARGO) build

$(KERNEL_REL):
	$(CARGO) build --release

$(INITRD):
	@cd test_root && test -f ../$(INITRD) || { \
	  echo "error: $(INITRD) not found — run 'make build' first"; \
	  exit 1; \
	}
