#![no_std]
#![no_main]
#![allow(unreachable_code)] // after schedule() -> ! and similar diverging calls


extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::global_asm;


pub mod drivers;
pub mod errno;
pub mod ipc;
pub mod mm;
pub mod namespace;
pub mod net;
pub mod plic;
pub mod posix;
pub mod smp;
pub mod syscall;
pub mod timer;
pub mod trace;
pub mod trap;
pub mod tty;
pub mod uart;
pub mod initrd;
pub mod sync;

static mut BOOT_DTB_PTR: usize = 0;

pub fn boot_dtb_ptr() -> usize {
    unsafe { BOOT_DTB_PTR }
}

/// Physical base address of the initrd (set in kmain from DTB, 0 if absent).
pub static INITRD_START: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
/// Length of the initrd in bytes.
pub static INITRD_LEN: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

// Include the boot assembly
global_asm!(include_str!("boot.s"));

// Halt loop that LLVM cannot optimize away — defined in global_asm so the
// compiler treats the call as an opaque side effect.
global_asm!(
    ".globl __halt_cpu",
    ".type __halt_cpu, @function",
    "__halt_cpu:",
    "  wfi",
    "  j __halt_cpu",
);
unsafe extern "C" {
    fn __halt_cpu() -> !;
}

#[unsafe(no_mangle)]
pub extern "C" fn kmain(hartid: usize, dtb_ptr: usize) -> ! {
    unsafe {
        BOOT_DTB_PTR = dtb_ptr;
    }
    // Detect UART base from DTB before the first print so the kernel works on
    // boards where the console isn't at the QEMU virt default (0x1000_0000).
    uart::init_from_dtb(dtb_ptr);

    crate::println!("Hello from openv!");
    crate::println!("Booted on hart ID: {}", hartid);
    crate::println!("DTB address: {:#x}", dtb_ptr);
    // Test raw put_char after println!
    {
        let mut u = uart::Uart::new();
        for &b in b"RAW_TEST_END\n" {
            u.put_char(b);
        }
    }

    // Initialize Memory Management
    mm::init(dtb_ptr);

    // Initialize root namespaces
    namespace::init();

    // Initialize Trap Handler
    trap::init();

    // Initialize networking via driver framework (virtio-mmio probe or loopback fallback)
    crate::net::init(dtb_ptr);

    // Initrd parsing
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

    // Test the global heap allocator
    let test_box = Box::new(42);
    crate::println!(
        "Successfully allocated Box on the heap. Value: {}",
        test_box
    );

    // Test VMO
    if let Some(vmo) = mm::vmo::Vmo::new(8192) {
        crate::println!(
            "Created VMO of size {} bytes across {} physical pages.",
            vmo.size(),
            vmo.pages().len()
        );
    } else {
        crate::println!("Failed to create VMO.");
    }

    // Test IPC Channels
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

    // Start secondary HARTs now that kernel data structures are ready
    smp::start_secondaries();
    trap::enable_timer(); // Moved here to enable preemption before spawning servers

    // Spawn boot servers
    crate::raw_print!("[HART-ONLY] kmain: attempting posix_spawn of boot servers\n");
    
    if let Ok(init_pid) = posix::spawn::posix_spawn("/init", 0) {
        // Boot servers inherit init's fds (and thus its TTY) so ACTIVE_TTY stays
        // pointing at init's TTY throughout boot.  ppid=0 would create a fresh TTY
        // per server and overwrite ACTIVE_TTY, leaving the shell on the wrong TTY.
        let pm_pid = posix::spawn::posix_spawn("/pm-server", init_pid).unwrap();
        let vfs_pid = posix::spawn::posix_spawn("/vfs-server", init_pid).unwrap();
        let rs_pid = posix::spawn::posix_spawn("/rs-server", init_pid).unwrap();
        
        let table = crate::posix::process::PROCESS_TABLE.lock();
        if let Some(init_proc) = table.get(&init_pid) {
            init_proc.caps.store(posix::process::CAP_NONE, core::sync::atomic::Ordering::Relaxed);
        }
        if let Some(pm_proc) = table.get(&pm_pid) {
            pm_proc.caps.store(posix::process::CAP_PROCESS | posix::process::CAP_DATACOPY, core::sync::atomic::Ordering::Relaxed);
        }
        if let Some(vfs_proc) = table.get(&vfs_pid) {
            vfs_proc.caps.store(
                posix::process::CAP_DATACOPY | posix::process::CAP_SYS_ADMIN,
                core::sync::atomic::Ordering::Relaxed,
            );
        }
        if let Some(rs_proc) = table.get(&rs_pid) {
            rs_proc.caps.store(
                posix::process::CAP_PROCESS | posix::process::CAP_SYS_ADMIN | posix::process::CAP_DATACOPY, 
                core::sync::atomic::Ordering::Relaxed
            );
        }
        drop(table);
        
        crate::raw_print!("[HART-ONLY] kmain: boot servers + /init spawn OK\n");
        crate::raw_print!("[HART-ONLY] Enabling timer, calling schedule()...\n");
        posix::process::schedule();
        crate::raw_print!("[HART-ONLY] SCHEDULE RETURNED! Hanging forever...\n");
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

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    crate::println!("KERNEL PANIC: {}", info);
    unsafe { __halt_cpu() };
}
