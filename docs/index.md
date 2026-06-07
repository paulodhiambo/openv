# openv Documentation

Welcome to the openv technical documentation. Start with the [README](../readme.md) for a project overview and quick-start, then dive into the docs below.

## Documents

| Document | Audience | Description |
|----------|----------|-------------|
| [readme.md](../readme.md) | Everyone | Quick start, build system, feature overview, status |
| [Architecture Index](architecture/README.md) | Kernel developers | Deep-dive: boot, MM, traps, VFS, IPC, net, SMP — 1,900+ lines |
| [syscalls.md](syscalls.md) | Userspace / libos developers | Every syscall: number, signature, return, semantics |
| [ROADMAP.md](ROADMAP.md) | Contributors / planners | What is done, in progress, and planned |
| [../CONTRIBUTING.md](CONTRIBUTING.md) | Contributors | Dev setup, coding standards, PR guide, good first issues |

---

## Quick Navigation by Subsystem

| Subsystem | Source | Architecture ref | Syscalls ref |
|-----------|--------|------------------|--------------|
| Boot & startup | `src/boot.s`, `src/main.rs` | [Boot](architecture/boot.md) | — |
| Physical memory | `src/mm/pmm.rs` | [PMM](architecture/mm.md#31-physical-memory-manager-pmm) | — |
| Virtual memory / paging | `src/mm/vmm.rs` | [VMM](architecture/mm.md#32-virtual-memory-manager-vmm) | — |
| Heap allocator | `src/mm/heap.rs` | [Heap](architecture/mm.md#33-heap) | — |
| Trap handler | `src/trap.rs` | [Traps](architecture/traps.md) | — |
| Process & scheduling | `src/posix/process.rs` | [Process Model](architecture/process.md) | [spawn, fork, exec, waitpid](syscalls.md#process--scheduling) |
| ELF loader | `src/posix/elf.rs` | [Fork/Exec](architecture/process.md#64-fork--exec--waitpid-semantics) | [exec](syscalls.md#process--scheduling) |
| Virtual File System | `src/vfs/` | [VFS](architecture/vfs.md) | [open, read, write, close](syscalls.md#file-descriptors) |
| IPC / handles / pipes | `src/ipc/` | [IPC](architecture/ipc.md) | [pipe, dup, dup2](syscalls.md#pipes) |
| TTY / terminal | `src/trap.rs` | [TTY](architecture/tty.md) | [set_echo, set_raw](syscalls.md#terminal-tty) |
| Networking | `src/net/` | [Networking](architecture/net.md) | [socket, bind, listen…](syscalls.md#networking) |
| SMP | `src/smp.rs`, `src/boot.s` | [SMP](architecture/smp.md) | — |
| User space / libos | `user/libos/` | [User Space](architecture/user.md) | [All syscalls](syscalls.md) |
| Security model | `src/posix/user.rs`, `src/vfs/mod.rs` | [Security](architecture/security.md) | [setuid, authenticate](syscalls.md#credentials--authentication) |
| CI / CD | `.github/workflows/build.yml` | [CI/CD](architecture/cicd.md) | — |

---

## Memory Map

```
Physical address space (QEMU virt machine)
  0x0000_0000 – 0x007F_FFFF   ROM / OpenSBI firmware
  0x0080_0000 – 0x0080_1FFF   OpenSBI jump pad
  0x0080_2000 …               Kernel (_start at 0x8020_0000)
  0x1000_0000                 NS16550 UART
  0x1000_8000                 Virtio-MMIO (NIC, blk…)
  0x0C00_0000                 PLIC
  0x8000_0000 – end-of-RAM    Heap, page tables, user processes

Virtual address space (Sv39, per-process)
  0x0000_0000 – 0xFFFF_FFFF   Kernel identity map  (1 GB superpages ×4, no PTE_U)
  0x1_0000_0000               User ELF load base
  0x1_xxxx_xxxx               User heap (grows up from ELF end)
  0x2_0000_0000               User stack top (USER_STACK_TOP, grows down)
```

---

## Glossary

| Term | Definition |
|------|------------|
| **HART** | Hardware Thread — a RISC-V CPU core / thread context |
| **Sv39** | RISC-V 39-bit virtual address paging: 3 levels of 512-entry 4 KiB page tables |
| **SBI** | Supervisor Binary Interface — M-mode firmware (OpenSBI) services |
| **COW** | Copy-on-Write — physical pages shared read-only; privatised on first write |
| **Vnode** | Virtual node — the `Vnode` trait object representing any filesystem entry |
| **TrapFrame** | Per-process saved register state (`kernel_sp · regs×32 · sepc · sstatus`) |
| **HandleTable** | Per-process file-descriptor table mapping `Handle (u32) → KernelObject` |
| **KernelObject** | Enum of openable resources: `Console · Channel · File · PipeRead · PipeWrite · Vmo` |
| **libos** | Userspace library providing `_start`, 2 MB heap, and all syscall wrappers |
| **initrd** | Initial RAM disk — a UStar tar archive parsed into MemFS at boot |
| **OFS** | openv's simple on-disk filesystem; mounted at `/mnt` when virtio-blk is present |
| **PMM** | Physical Memory Manager — free-list allocator + per-page `u16` refcounts |
| **VMM** | Virtual Memory Manager — per-process Sv39 page table management |
| **PLIC** | Platform-Level Interrupt Controller — routes external IRQs on RISC-V |
| **sscratch** | RISC-V S-mode scratch CSR — holds the TrapFrame pointer in openv |
| **sepc** | Supervisor Exception Program Counter — saved PC on trap entry |
| **sstatus** | Supervisor Status CSR — SPP, SPIE, SIE bits |
| **WFI** | Wait For Interrupt — idle instruction; CPU halts until next IRQ |
