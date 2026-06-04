# openv Documentation

Welcome to the openv technical documentation. Start with the [README](../readme.md) for a project overview and quick-start, then dive into the docs below.

## Documents

| Document | Audience | Description |
|----------|----------|-------------|
| [readme.md](../readme.md) | Everyone | Quick start, build system, feature overview, status |
| [architecture.md](architecture.md) | Kernel developers | Deep-dive: boot, MM, traps, VFS, IPC, net, SMP — 1,900+ lines |
| [syscalls.md](syscalls.md) | Userspace / libos developers | Every syscall: number, signature, return, semantics |
| [ROADMAP.md](ROADMAP.md) | Contributors / planners | What is done, in progress, and planned |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | Contributors | Dev setup, coding standards, PR guide, good first issues |

---

## Quick Navigation by Subsystem

| Subsystem | Source | Architecture ref | Syscalls ref |
|-----------|--------|------------------|--------------|
| Boot & startup | `src/boot.s`, `src/main.rs` | [§2 Boot](architecture.md#2-boot-sequence) | — |
| Physical memory | `src/mm/pmm.rs` | [§3.1 PMM](architecture.md#31-physical-memory-manager-pmm) | — |
| Virtual memory / paging | `src/mm/vmm.rs` | [§3.2 VMM](architecture.md#32-virtual-memory-manager-vmm) | — |
| Heap allocator | `src/mm/heap.rs` | [§3.3 Heap](architecture.md#33-heap) | — |
| Trap handler | `src/trap.rs` | [§4 Traps](architecture.md#4-trap-handling) | — |
| Process & scheduling | `src/posix/process.rs` | [§6 Process Model](architecture.md#6-process-model) | [spawn, fork, exec, waitpid](syscalls.md#process--scheduling) |
| ELF loader | `src/posix/elf.rs` | [§6.4](architecture.md#64-forkexecwaitpid-semantics) | [exec](syscalls.md#process--scheduling) |
| Virtual File System | `src/vfs/` | [§7 VFS](architecture.md#7-virtual-file-system) | [open, read, write, close](syscalls.md#file-descriptors) |
| IPC / handles / pipes | `src/ipc/` | [§8 IPC](architecture.md#8-inter-process-communication-ipc) | [pipe, dup, dup2](syscalls.md#pipes) |
| TTY / terminal | `src/trap.rs` | [§10 TTY](architecture.md#10-tty--line-discipline) | [set_echo, set_raw](syscalls.md#terminal-tty) |
| Networking | `src/net/` | [§9 Networking](architecture.md#9-networking) | [socket, bind, listen…](syscalls.md#networking) |
| SMP | `src/smp.rs`, `src/boot.s` | [§11 SMP](architecture.md#11-symmetric-multi-processing-smp) | — |
| User space / libos | `user/libos/` | [§12 User Space](architecture.md#12-user-space) | [All syscalls](syscalls.md) |
| Security model | `src/posix/user.rs`, `src/vfs/mod.rs` | [§13 Security](architecture.md#13-security-model) | [setuid, authenticate](syscalls.md#credentials--authentication) |
| CI / CD | `.github/workflows/build.yml` | [§15 CI/CD](architecture.md#15-cicd) | — |

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
