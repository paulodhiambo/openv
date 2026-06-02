#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::panic::PanicInfo;

pub mod ipc;
pub mod mm;
pub mod posix;
pub mod trap;
pub mod uart;
pub mod vfs;
pub mod net;
pub mod plic;

static mut BOOT_DTB_PTR: usize = 0;

pub fn boot_dtb_ptr() -> usize {
    unsafe { BOOT_DTB_PTR }
}

// Include the boot assembly
global_asm!(include_str!("boot.s"));

#[unsafe(no_mangle)]
pub extern "C" fn kmain(hartid: usize, dtb_ptr: usize) -> ! {
    unsafe { BOOT_DTB_PTR = dtb_ptr; }
    // Initialize UART
    let mut uart = uart::Uart::new();
    uart.init();

    println!("Hello from openv!");
    println!("Booted on hart ID: {}", hartid);
    println!("DTB address: {:#x}", dtb_ptr);

    // Initialize Memory Management
    mm::init(dtb_ptr);

    // Initialize Trap Handler
    trap::init();

    // Initialize networking (virtio-mmio probe or loopback)
    crate::net::init();

    // Initrd parsing
    let fdt = unsafe { fdt::Fdt::from_ptr(dtb_ptr as *const u8).unwrap() };
    if let Some(chosen) = fdt.find_node("/chosen") {
        if let (Some(start_prop), Some(end_prop)) = (chosen.property("linux,initrd-start"), chosen.property("linux,initrd-end")) {
            let start = start_prop.as_usize().unwrap_or(0);
            let end = end_prop.as_usize().unwrap_or(0);
            
            if start > 0 && end > start {
                println!("Found initrd at {:#x} - {:#x} ({} bytes)", start, end, end - start);
                let initrd_slice = unsafe { core::slice::from_raw_parts(start as *const u8, end - start) };
                let root_fs = vfs::tar::parse_tar(initrd_slice);
                vfs::MOUNT_TABLE.lock().root = Some(root_fs);
                println!("VFS: Mounted MemFS at / from initrd.");
            }
        }
    } else {
        println!("No initrd found in DTB.");
    }

    // Test VFS reading
    println!("--- Testing VFS ---");
    if let Ok(file) = vfs::lookup_path("/dummy.txt") {
        let mut buf = [0u8; 32];
        if let Ok(bytes_read) = file.read(0, &mut buf) {
            if let Ok(s) = core::str::from_utf8(&buf[..bytes_read]) {
                println!("Successfully read from /dummy.txt: '{}'", s);
            }
        }
    } else {
        println!("Failed to find /dummy.txt");
    }

    // Test the global heap allocator
    let test_box = Box::new(42);
    println!("Successfully allocated Box on the heap. Value: {}", test_box);

    // Test VMO
    if let Some(vmo) = mm::vmo::Vmo::new(8192) {
        println!("Created VMO of size {} bytes across {} physical pages.", vmo.size(), vmo.pages().len());
    } else {
        println!("Failed to create VMO.");
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
        println!("Wrote message to Endpoint 1.");
    }

    if let Some(received_msg) = ep2.try_recv() {
        if let Ok(s) = core::str::from_utf8(&received_msg.bytes) {
            println!("Endpoint 2 received message: '{}'", s);
        }
    } else {
        println!("Endpoint 2 failed to receive message.");
    }

    // Test POSIX Process logic
    println!("--- Testing POSIX spawn and waitpid ---");
    let init_pid = posix::process::Process::new(0).pid; // Mock init process
    
    // Spawn the user shell directly as PID 1's child to provide interactive console
    if let Ok(child_pid) = posix::spawn::posix_spawn("/sh", init_pid) {
        println!("Jumping to Scheduler...");
        crate::posix::process::schedule();
    } else {
        println!("Failed to spawn /sh. Check initrd.");
    }

    // Test trap handler (this will panic and demonstrate the handler works)
    println!("--- Testing ecall trap ---");
    unsafe {
        core::arch::asm!("ecall");
    }

    // Halt loop
    loop {
        // Wait for interrupt (to save power)
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("KERNEL PANIC: {}", info);
    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}
