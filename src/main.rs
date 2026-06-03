#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::panic::PanicInfo;

pub mod block;
pub mod drivers;
pub mod ipc;
pub mod mm;
pub mod net;
pub mod plic;
pub mod posix;
pub mod smp;
pub mod timer;
pub mod trap;
pub mod uart;
pub mod vfs;

static mut BOOT_DTB_PTR: usize = 0;

pub fn boot_dtb_ptr() -> usize {
    unsafe { BOOT_DTB_PTR }
}

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
    // Raw UART test: manually write "hartid=N" bypassing fmt
    {
        let mut u = uart::Uart::new();
        for &b in b"hartid=" {
            u.put_char(b);
        }
        u.put_char(b'0' + hartid as u8);
        u.put_char(b'\r');
        u.put_char(b'\n');
    }
    // Test raw put_char after initialization
    {
        let mut u = uart::Uart::new();
        for &b in b"RAW_TEST_START\n" {
            u.put_char(b);
        }
    }
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
                crate::println!(
                    "Found initrd at {:#x} - {:#x} ({} bytes)",
                    start,
                    end,
                    end - start
                );
                let initrd_slice =
                    unsafe { core::slice::from_raw_parts(start as *const u8, end - start) };
                let root_fs = vfs::tar::parse_tar(initrd_slice);
                {
                    let mut mt = vfs::MOUNT_TABLE.lock();
                    mt.root = Some(root_fs);
                    // Mount /proc (ProcFS) and /dev (DevFS)
                    mt.mounts.push((
                        alloc::string::String::from("/proc"),
                        Arc::new(vfs::procfs::ProcRoot),
                    ));
                    mt.mounts.push((
                        alloc::string::String::from("/dev"),
                        Arc::new(vfs::devfs::DevRoot),
                    ));
                }
                crate::println!("VFS: Mounted MemFS at /, ProcFS at /proc, DevFS at /dev.");

                // Mount persistent OFS filesystem at /mnt if a block device is present.
                if let Some(ofs_root) = vfs::blockfs::try_mount() {
                    let mut mt = vfs::MOUNT_TABLE.lock();
                    // Ensure /mnt exists in the initrd tarfs
                    mt.mounts.push((
                        alloc::string::String::from("/mnt"),
                        ofs_root,
                    ));
                    crate::println!("VFS: mounted OFS at /mnt");
                }
            }
        }
    } else {
        crate::println!("No initrd found in DTB.");
    }

    // Test VFS reading
    crate::println!("--- Testing VFS ---");
    if let Ok(file) = vfs::lookup_path("/dummy.txt") {
        let mut buf = [0u8; 32];
        if let Ok(bytes_read) = file.read(0, &mut buf) {
            if let Ok(s) = core::str::from_utf8(&buf[..bytes_read]) {
                crate::println!("Successfully read from /dummy.txt: '{}'", s);
            }
        }
    } else {
        crate::println!("Failed to find /dummy.txt");
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
    crate::raw_print!("[HART-ONLY] kmain: about to create init process\n");
    let init_pid = posix::process::Process::new(0).pid; // Mock init process

    // Start secondary HARTs now that kernel data structures are ready
    smp::start_secondaries();

    // Spawn init (PID 1) which will manage the lifecycle of all user processes
    crate::raw_print!("[HART-ONLY] kmain: attempting posix_spawn(/init)\n");
    if let Ok(_init_pid) = posix::spawn::posix_spawn("/init", init_pid) {
        crate::raw_print!("[HART-ONLY] kmain: /init spawn OK\n");
        crate::raw_print!("[HART-ONLY] Enabling timer, calling schedule()...\n");
        trap::enable_timer();
        posix::process::schedule();
        crate::raw_print!("[HART-ONLY] SCHEDULE RETURNED! Hanging forever...\n");
        unsafe { __halt_cpu() };
    } else if let Ok(_) = posix::spawn::posix_spawn("/sh", init_pid) {
        crate::raw_print!("[HART-ONLY] kmain: /init FAILED, /sh spawn OK\n");
        crate::raw_print!("[HART-ONLY] Calling schedule() (fallback)...\n");
        posix::process::schedule();
        crate::raw_print!("[HART-ONLY] SCHEDULE RETURNED (fallback)! Hanging forever...\n");
        unsafe { __halt_cpu() };
    } else {
        crate::raw_print!("[HART-ONLY] kmain: BOTH /init AND /sh FAILED\n");
    }

    crate::raw_print!("[HART-ONLY] kmain: AFTER if-else chain (will test ecall trap)\n");
    crate::println!("--- Testing ecall trap ---");
    unsafe {
        core::arch::asm!("ecall");
    }

    unsafe { __halt_cpu() };
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    crate::raw_print!("KERNEL PANIC: ");
    if let Some(msg) = info.message().as_str() {
        crate::uart::write_str(msg);
    } else {
        crate::uart::write_str("(formatted message)");
    }
    if let Some(loc) = info.location() {
        crate::uart::write_str(" at ");
        crate::uart::write_str(loc.file());
        crate::uart::write_str(":");
        crate::uart::write_dec(loc.line() as usize);
    }
    crate::uart::write_str("\n");
    unsafe { __halt_cpu() };
}
