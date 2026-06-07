//! # OpenV Microkernel
//!
//! OpenV is a RISC-V microkernel operating system written in Rust. It is designed
//! to be a small, efficient, and secure kernel for RISC-V hardware, with a focus
//! on capability-based security, message-passing IPC, and modularity.
//!
//! ## Architecture
//!
//! OpenV is structured as a microkernel, meaning that most of the operating system
//! functionality is implemented in user-space servers. The kernel provides only the
//! most basic services:
//!
//! - **Memory management**: Physical and virtual memory allocation, page tables,
//!   and memory-mapped I/O.
//! - **Inter-process communication (IPC)**: Message passing via channels and
//!   mailboxes.
//! - **Process scheduling**: Preemptive, priority-based scheduling of threads
//!   across multiple HARTs (hardware threads).
//! - **Interrupt handling**: Trap and interrupt dispatching, including timer
//!   interrupts for preemption.
//! - **Capability-based security**: Fine-grained access control via capabilities
//!   that can be passed over IPC channels.
//!
//! ## Boot Process
//!
//! The kernel boots from `boot.s`, which sets up the stack and calls [`kmain`].
//! The [`kmain`] function is the entry point of the kernel and is responsible
//! for initializing all kernel subsystems and starting the boot servers.
//!
//! ## Module Organization
//!
//! The kernel is organized into the following modules:
//!
//! - [`drivers`]: Device drivers for UART, block devices, network devices, etc.
//! - [`errno`]: Error codes used throughout the kernel.
//! - [`ipc`]: Inter-process communication primitives (channels, mailboxes).
//! - [`mm`]: Memory management (physical allocator, virtual memory, page tables).
//! - [`namespace`]: Namespace support for isolating system resources.
//! - [`net`]: Networking stack and device drivers.
//! - [`plic`]: Platform-Level Interrupt Controller driver.
//! - [`posix`]: POSIX-like process management, file system, and IPC syscalls.
//! - [`smp`]: Symmetric multiprocessing support for multi-HART systems.
//! - [`sync`]: Synchronization primitives (spinlocks, semaphores).
//! - [`syscall`]: System call implementations.
//! - [`timer`]: Timer driver and timekeeping.
//! - [`trace`]: Tracing and logging infrastructure.
//! - [`trap`]: Trap and interrupt handling.
//! - [`tty`]: TTY (terminal) support.
//! - [`uart`]: UART (serial port) driver.
//! - [`initrd`]: Initial ramdisk (initrd) support.
//!
//! ## References
//!
//! For more information about the RISC-V architecture, see the
//! [RISC-V ISA Specification](https://riscv.org/technical/specifications/).
//!
//! [`kmain`]: fn.kmain.html

#![no_std]
#![no_main]
#![allow(unreachable_code)] // after schedule() -> ! and similar diverging calls


extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::global_asm;


/// Module containing device drivers for various hardware devices.
pub mod drivers;
/// Module containing error codes used throughout the kernel.
pub mod errno;
/// Module containing inter-process communication primitives.
pub mod ipc;
/// Module containing memory management code.
pub mod mm;
/// Module containing namespace support.
pub mod namespace;
/// Module containing networking code.
pub mod net;
/// Module containing the Platform-Level Interrupt Controller driver.
pub mod plic;
/// Module containing POSIX-like process management and syscalls.
pub mod posix;
/// Module containing symmetric multiprocessing support.
pub mod smp;
/// Module containing system call implementations.
pub mod syscall;
/// Module containing timer driver and timekeeping.
pub mod timer;
/// Module containing tracing and logging infrastructure.
pub mod trace;
/// Module containing trap and interrupt handling.
pub mod trap;
/// Module containing TTY (terminal) support.
pub mod tty;
/// Module containing UART (serial port) driver.
pub mod uart;
/// Module containing initial ramdisk (initrd) support.
pub mod initrd;
/// Module containing synchronization primitives.
pub mod sync;

/// Atomic storage for the device tree blob (DTB) pointer passed by the bootloader.
///
/// This pointer is set once in [`kmain`] from the `dtb_ptr` argument and never
/// modified thereafter. It is stored as an [`AtomicUsize`] to allow safe access
/// from any context without requiring `&mut` on a `static`.
///
/// [`AtomicUsize`]: core::sync::atomic::AtomicUsize
/// [`kmain`]: fn.kmain.html
static BOOT_DTB_PTR: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Returns the physical address of the device tree blob (DTB) passed by the
/// bootloader.
///
/// The DTB is a data structure that describes the hardware configuration of
/// the platform. It is used during kernel initialization to discover devices
/// and configure drivers.
///
/// # Returns
///
/// The physical address of the DTB, or `0` if no DTB was provided.
///
/// # Example
///
/// ```ignore
/// let dtb = boot_dtb_ptr();
/// if dtb != 0 {
///     // Parse the DTB...
/// }
/// ```
pub fn boot_dtb_ptr() -> usize {
    BOOT_DTB_PTR.load(core::sync::atomic::Ordering::Relaxed)
}

/// Physical base address of the initrd (set in [`kmain`] from DTB, 0 if absent).
///
/// The initrd is an initial ramdisk that contains the root file system and
/// boot-time utilities. This atomic stores the physical base address of the
/// initrd, which is set during kernel initialization from the `linux,initrd-start`
/// property of the `/chosen` node in the DTB.
///
/// [`kmain`]: fn.kmain.html
pub static INITRD_START: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
/// Length of the initrd in bytes.
///
/// This atomic stores the length of the initrd in bytes, which is set during
/// kernel initialization from the `linux,initrd-end` and `linux,initrd-start`
/// properties of the `/chosen` node in the DTB.
pub static INITRD_LEN: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// Include the boot assembly. The boot.s file contains the initial boot code
// that sets up the stack and calls kmain. It is written in RISC-V assembly
// and is platform-specific.
global_asm!(include_str!("boot.s"));

// Halt loop that LLVM cannot optimize away — defined in global_asm so the
// compiler treats the call as an opaque side effect.
//
// This assembly routine is used to halt the CPU in an infinite loop. It is
// used in panic handlers and after the kernel has finished initialization.
// The `wfi` instruction waits for an interrupt, which will never come in a
// halted CPU, and the `j __halt_cpu` creates an infinite loop.
global_asm!(
    ".globl __halt_cpu",
    ".type __halt_cpu, @function",
    "__halt_cpu:",
    "  wfi",
    "  j __halt_cpu",
);

// Defined above in global_asm; wfi loop that LLVM cannot eliminate.
unsafe extern "C" {
    fn __halt_cpu() -> !;
}

/// Kernel main entry point.
///
/// This function is called from the boot assembly after the stack has been
/// set up. It is responsible for initializing all kernel subsystems and
/// starting the boot servers. It never returns.
///
/// # Arguments
///
/// * `hartid` - The hardware thread (HART) ID of the current CPU. This is
///   used to identify which CPU is running the kernel. The primary HART
///   (usually HART 0) is responsible for initializing the system and
///   starting secondary HARTs.
/// * `dtb_ptr` - The physical address of the device tree blob (DTB) passed
///   by the bootloader. The DTB describes the hardware configuration of
///   the platform and is used to discover devices and configure drivers.
///
/// # Returns
///
/// This function never returns (`!`). After initializing the kernel and
/// starting the boot servers, it enters the scheduler via
/// [`posix::process::schedule`], which never returns. If the scheduler
/// does return (which should never happen), the CPU is halted via
/// [`__halt_cpu`].
///
/// # Boot Sequence
///
/// The boot sequence is as follows:
///
/// 1. Store the DTB pointer in [`BOOT_DTB_PTR`].
/// 2. Initialize the UART from the DTB so the console works on platforms
///    where the UART isn't at the QEMU virt default address.
/// 3. Print boot messages to the console.
/// 4. Initialize memory management ([`mm::init`]).
/// 5. Initialize root namespaces ([`namespace::init`]).
/// 6. Initialize the trap handler ([`trap::init`]).
/// 7. Initialize networking ([`net::init`]).
/// 8. Parse the initrd from the DTB and store its location in
///    [`INITRD_START`] and [`INITRD_LEN`].
/// 9. Test the global heap allocator and basic kernel primitives.
/// 10. Start secondary HARTs ([`smp::start_secondaries`]).
/// 11. Enable the timer for preemption ([`trap::enable_timer`]).
/// 12. Spawn the boot servers (`/init`, `/pm-server`, `/vfs-server`,
///     `/rs-server`).
/// 13. Enter the scheduler ([`posix::process::schedule`]).
///
/// # Safety
///
/// This function is called from assembly and must not be called directly.
/// It is marked as `#[unsafe(no_mangle)]` to ensure that the symbol is
/// not mangled by the Rust compiler.
///
/// [`BOOT_DTB_PTR`]: static.BOOT_DTB_PTR.html
/// [`__halt_cpu`]: fn.__halt_cpu.html
/// [`mm::init`]: mm/fn.init.html
/// [`namespace::init`]: namespace/fn.init.html
/// [`trap::init`]: trap/fn.init.html
/// [`net::init`]: net/fn.init.html
/// [`smp::start_secondaries`]: smp/fn.start_secondaries.html
/// [`trap::enable_timer`]: trap/fn.enable_timer.html
/// [`posix::process::schedule`]: posix/process/fn.schedule.html
/// [`INITRD_START`]: static.INITRD_START.html
/// [`INITRD_LEN`]: static.INITRD_LEN.html
#[unsafe(no_mangle)]
pub extern "C" fn kmain(hartid: usize, dtb_ptr: usize) -> ! {
    // Store the DTB pointer for later use. We use Relaxed ordering because
    // this is the only store to this location.
    BOOT_DTB_PTR.store(dtb_ptr, core::sync::atomic::Ordering::Relaxed);
    // Detect UART base from DTB before the first print so the kernel works on
    // boards where the console isn't at the QEMU virt default (0x1000_0000).
    uart::init_from_dtb(dtb_ptr);

    // Print boot messages to the console. The `println!` macro writes to the
    // UART, which is the primary console output device.
    crate::println!("Hello from openv!");
    crate::println!("Booted on hart ID: {}", hartid);
    crate::println!("DTB address: {:#x}", dtb_ptr);
    // Test raw put_char after println! to verify that the UART driver works
    // correctly when called directly.
    {
        let mut u = uart::Uart::new();
        for &b in b"RAW_TEST_END\n" {
            u.put_char(b);
        }
    }

    // Initialize Memory Management. This sets up the physical memory allocator,
    // the kernel page table, and the virtual memory subsystem.
    mm::init(dtb_ptr);

    // Initialize root namespaces. Namespaces provide isolation for system
    // resources such as PIDs, mount points, and network interfaces.
    namespace::init();

    // Initialize Trap Handler. This sets up the trap vector and configures
    // the CPU to handle traps (exceptions, interrupts, and system calls).
    trap::init();

    // Initialize networking via driver framework (virtio-mmio probe or loopback fallback).
    // This probes the DTB for network devices and initializes the appropriate
    // driver. If no network device is found, a loopback driver is used as a
    // fallback.
    crate::net::init(dtb_ptr);

    // Initrd parsing. The initrd is described in the DTB's `/chosen` node
    // via the `linux,initrd-start` and `linux,initrd-end` properties.
    let fdt = unsafe { fdt::Fdt::from_ptr(dtb_ptr as *const u8).unwrap() };
    if let Some(chosen) = fdt.find_node("/chosen") {
        if let (Some(start_prop), Some(end_prop)) = (
            chosen.property("linux,initrd-start"),
            chosen.property("linux,initrd-end"),
        ) {
            let start = start_prop.as_usize().unwrap_or(0);
            let end = end_prop.as_usize().unwrap_or(0);

            if start > 0 && end > start {
                let initrd_len = end - start;
                let initrd_slice = unsafe { core::slice::from_raw_parts(start as *const u8, initrd_len) };
                crate::raw_print!("[HART-ONLY] Found initrd\n");    unsafe { crate::initrd::init(initrd_slice); }

                // Store for sys_initrd_data (syscall 65) so the VFS server can fetch it.
                INITRD_START.store(start, core::sync::atomic::Ordering::Relaxed);
                INITRD_LEN.store(end - start, core::sync::atomic::Ordering::Relaxed);
            }
        }
    } else {
        crate::println!("No initrd found in DTB.");
    }

    // Test the global heap allocator. This verifies that the heap allocator
    // is working correctly before we start using it for kernel allocations.
    let test_box = Box::new(42);
    crate::println!(
        "Successfully allocated Box on the heap. Value: {}",
        test_box
    );

    // Test VMO. A VMO (Virtual Memory Object) is the kernel's abstraction
    // for a contiguous region of memory. This test creates an 8KB VMO and
    // prints its size and number of physical pages.
    if let Some(vmo) = mm::vmo::Vmo::new(8192) {
        crate::println!(
            "Created VMO of size {} bytes across {} physical pages.",
            vmo.size(),
            vmo.pages().len()
        );
    } else {
        crate::println!("Failed to create VMO.");
    }

    // Test IPC Channels. IPC channels are the primary mechanism for
    // inter-process communication in OpenV. This test creates a pair of
    // endpoints, sends a message from one to the other, and receives it.
    let (ep1, ep2) = ipc::channel::ChannelEndpoint::create_pair();

    let msg_str = b"Hello from Endpoint 1!";
    let mut msg_bytes = Vec::new();
    msg_bytes.extend_from_slice(msg_str);

    let msg = ipc::channel::Message {
        bytes: msg_bytes,
        handles: Vec::new(),
    };

    if ep1.write(msg).is_ok() {
        crate::println!("Wrote message to Endpoint 1.");
    }

    if let Some(received_msg) = ep2.try_recv() {
        if let Ok(s) = core::str::from_utf8(&received_msg.bytes) {
            crate::println!("Endpoint 2 received message: '{}'", s);
        }
    } else {
        crate::println!("Endpoint 2 failed to receive message.");
    }

    // Test POSIX Process logic
    crate::raw_print!("[HART-ONLY] kmain: about to create boot servers\n");

    // Start secondary HARTs now that kernel data structures are ready.
    // The primary HART has finished initializing the kernel, so it's safe
    // to start the secondary HARTs, which will begin executing user-space
    // processes.
    smp::start_secondaries();
    // Moved here to enable preemption before spawning servers. This enables
    // the timer interrupt, which is used for preemptive scheduling.
    trap::enable_timer();

    // Spawn boot servers
    crate::raw_print!("[HART-ONLY] kmain: attempting posix_spawn of boot servers\n");
    
    if let Ok(init_pid) = posix::spawn::posix_spawn("/init", 0) {
        // Boot servers inherit init's fds (and thus its TTY) so ACTIVE_TTY stays
        // pointing at init's TTY throughout boot.  ppid=0 would create a fresh TTY
        // per server and overwrite ACTIVE_TTY, leaving the shell on the wrong TTY.
        let pm_pid = posix::spawn::posix_spawn("/pm-server", init_pid).unwrap();
        let vfs_pid = posix::spawn::posix_spawn("/vfs-server", init_pid).unwrap();
        let rs_pid = posix::spawn::posix_spawn("/rs-server", init_pid).unwrap();
        
        // Set capabilities for each boot server. Capabilities determine what
        // operations a process is allowed to perform. Each server gets a
        // different set of capabilities based on its role.
        let table = crate::posix::process::PROCESS_TABLE.lock();
        if let Some(init_proc) = table.get(&init_pid) {
            // The init process has no special capabilities.
            init_proc.caps.store(posix::process::CAP_NONE, core::sync::atomic::Ordering::Relaxed);
        }
        if let Some(pm_proc) = table.get(&pm_pid) {
            // The process manager (pm-server) can create and manage processes
            // and copy data between processes.
            pm_proc.caps.store(posix::process::CAP_PROCESS | posix::process::CAP_DATACOPY, core::sync::atomic::Ordering::Relaxed);
        }
        if let Some(vfs_proc) = table.get(&vfs_pid) {
            // The virtual file system server (vfs-server) can copy data and
            // perform system administration tasks.
            vfs_proc.caps.store(
                posix::process::CAP_DATACOPY | posix::process::CAP_SYS_ADMIN,
                core::sync::atomic::Ordering::Relaxed,
            );
        }
        if let Some(rs_proc) = table.get(&rs_pid) {
            // The resource server (rs-server) can manage processes, perform
            // system administration, and copy data.
            rs_proc.caps.store(
                posix::process::CAP_PROCESS | posix::process::CAP_SYS_ADMIN | posix::process::CAP_DATACOPY, 
                core::sync::atomic::Ordering::Relaxed
            );
        }
        drop(table);
        
        crate::raw_print!("[HART-ONLY] kmain: boot servers + /init spawn OK\n");
        crate::raw_print!("[HART-ONLY] Enabling timer, calling schedule()...\n");
        // Enter the scheduler. This function never returns.
        posix::process::schedule();
        crate::raw_print!("[HART-ONLY] SCHEDULE RETURNED! Hanging forever...\n");
        // If the scheduler does return (which should never happen), halt the CPU.
        unsafe { __halt_cpu() };
    } else if let Err(e) = posix::spawn::posix_spawn("/init", 0) {
        crate::println!("[HART-ONLY] kmain: /init FAILED: {}", e);
    }

    crate::raw_print!("[HART-ONLY] kmain: AFTER if-else chain (will test ecall trap)\n");
    crate::println!("--- Testing ecall trap ---");
    unsafe {
        core::arch::asm!("ecall");
    }

    unsafe { __halt_cpu() };
}

/// Kernel panic handler.
///
/// This function is called when the kernel panics. It prints the panic
/// information to the console and halts the CPU. It never returns.
///
/// # Arguments
///
/// * `info` - Information about the panic, including the panic message
///   and the location where the panic occurred.
///
/// # Returns
///
/// This function never returns (`!`). After printing the panic
/// information, it halts the CPU via [`__halt_cpu`].
///
/// # Safety
///
/// This function is the panic handler for the entire kernel. It is
/// called by the Rust runtime when a panic occurs. It must not be
/// called directly.
///
/// [`__halt_cpu`]: fn.__halt_cpu.html
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crate::println!("KERNEL PANIC: {}", info);
    unsafe { __halt_cpu() };
}
