# OpenV Development Roadmap

This document outlines the strategic path to achieving a functional, "Linux-level" OS with GUI capabilities.

See docs/syscall.md for the current syscall ABI and conventions.

## Milestones

### Phase 1: Foundation (POSIX & Syscalls)
*Goal: Create a stable API for userland applications.*
- [ ] Stabilize Syscall ABI.
- [ ] Implement `fork` (with Copy-on-Write memory).
- [ ] Implement `exec` and robust `wait` system.
- [ ] Port/Implement minimal `no_std` compatible C library (e.g., musl subset).
- [ ] Implement PID 1 (Init system).

### Phase 2: Kernel Architecture Maturity
*Goal: Build a production-grade, preemptive multitasking kernel.*
- [ ] Support Symmetric Multiprocessing (SMP).
- [ ] Implement Demand Paging and robust memory management.
- [ ] Implement a unified Virtual File System (VFS) abstraction.
- [ ] Define a device driver framework (dynamic loading/discovery).

### Phase 3: GUI & Input Hardware
*Goal: Enable interactive graphical user interface.*
- [ ] Framebuffer Driver (VirtIO).
- [ ] Input Driver (Keyboard/Mouse via VirtIO).
- [ ] Integration of `embedded-graphics` for drawing primitives.
- [ ] Basic Window Manager/Compositor.

### Phase 4: System Services
*Goal: Provide a complete runtime environment.*
- [ ] Networking stack (POSIX Sockets via `smoltcp` port).
- [ ] Dynamic Linker (loading shared libraries).

---

## Actionable Next Steps

1.  **Research:** Analyze `scripts/run.sh` to determine how your emulator (QEMU) exposes hardware devices (display/input).
2.  **Research:** Study `xv6-riscv` architecture for established patterns in syscall/process management.
3.  **Task:** Define the stable Syscall ABI to ensure user-space programs can interact with the kernel consistently.
4.  **Task:** Prototype a minimal framebuffer driver to get basic pixel data on the screen.
